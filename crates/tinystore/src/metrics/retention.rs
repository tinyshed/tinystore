//! What names keep no longer: each series is due at its oldest sample plus
//! its name's `keep`, one indexed value a series, and a pass takes a bounded
//! batch of what is due. A head's expired samples go at once; a block goes
//! once its last sample has expired, and a group once it holds none; a group
//! partly expired keeps every payload's address, its live mask telling which
//! still count. A series left with nothing goes with its postings and the
//! dictionary pairs no other series uses, and frees its place among the
//! store's series.

use rusqlite::{Connection, OptionalExtension, params};

use super::clock;
use super::engine::{Engine, execute, query_one};
use super::head::{self, Extent};
use super::ingest::ready;
use super::labels::decode_ids;
use super::names::{KEEP, millis};
use super::publish::{self, earlier, group_row, read_group};
use crate::compress::frame::Zstd;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, ErrorKind, Result};

/// The series a pass looks at at most, so that a pass stays short; another
/// follows at once while one leaves work.
pub(super) const PASS_SERIES: usize = 64;

/// A series due: its id, and the oldest time its name keeps.
#[derive(Clone, Copy, Debug)]
pub(super) struct Due {
    id: i64,
    cutoff: i64,
    keep: i64,
}

/// What expiry did.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Expired {
    pub(super) samples: u64,
    pub(super) reclaimed: u64,
}

const DUE: &str = "select st.series_id, n.name from series_state st
    join series s on s.id = st.series_id join names n on n.id = s.name
    where st.due is not null and st.due <= ?1 and st.failed_at is null
    order by st.due, st.series_id limit cast(?2 as integer)";
const HEAD: &str = "select st.version, st.max_seen, st.head_count, st.head_start, st.head_end, h.body
    from series_state st left join heads h on h.series_id = st.series_id where st.series_id = ?1";
const SAVEPOINT: &str = "savepoint metrics_expiry";
const RELEASE: &str = "release metrics_expiry";
const ROLLBACK_TO: &str = "rollback to metrics_expiry";
const SUSPEND: &str = "update series_state set failed_at = ?2, failure = ?3 where series_id = ?1 and failed_at is null";
const SAVE_HEAD: &str = "update heads set body = ?2 where series_id = ?1";
const DROP_HEAD: &str = "delete from heads where series_id = ?1";
const SAVE_STATE: &str = "update series_state set version = version + 1, ready = ?2,
    head_count = ?3, head_start = ?4, head_end = ?5 where series_id = ?1";
/// A series retention took samples of says so from then on: the step from
/// the last of them into its oldest sample is one no read counts.
const SET_DUE: &str = "update series_state set due = ?2, trimmed = max(trimmed, ?3), oldest = ?4 where series_id = ?1";
const HAS_GROUPS: &str = "select exists (select 1 from groups where series_id = ?1)";
const OLDEST: &str = "select id, start_ts, end_ts, clock, directory from groups
    where series_id = ?1 order by start_ts limit 1";
const DROP_PAYLOAD: &str = "delete from payloads where id = ?1";
const DROP_GROUP: &str = "delete from groups where id = ?1";
const REWRITE_GROUP: &str = "update groups set directory = ?2 where id = ?1";

impl Engine {
    /// The series whose next sample is past what their names keep, a pass's
    /// worth, the most overdue first.
    pub(super) fn due(&self, now: i64) -> Result<Vec<Due>> {
        let failed = |error| sql_error("find what names keep no longer", error);
        let named: Vec<(i64, String)> = self.file.read(|connection| {
            let mut select = connection.prepare_cached(DUE).map_err(failed)?;
            let rows = select.query_map(params![now, PASS_SERIES as i64], |row| Ok((row.get(0)?, row.get(1)?)));
            rows.and_then(Iterator::collect).map_err(failed)
        })?;
        Ok(named
            .into_iter()
            .map(|(id, name)| {
                let keep = millis(self.names.get(&name).map_or(KEEP, |name| name.keeps()));
                Due { id, cutoff: now.saturating_sub(keep), keep }
            })
            .collect())
    }

    /// Removes what each series due keeps no longer, in one write, each series
    /// in a savepoint of its own: one whose bytes no longer read is suspended,
    /// as publication suspends it, rather than stall every expiry after it.
    pub(super) fn expire(&self, due: Vec<Due>) -> Result<Expired> {
        if due.is_empty() {
            return Ok(Expired::default());
        }
        let zstd = self.codecs.take();
        let (zstd, expired) = self.file.write(0, move |tx| {
            let mut zstd = zstd;
            let mut expired = Expired::default();
            for series in &due {
                if let Some(done) = expire_isolated(tx, &mut zstd, series)? {
                    expired.samples += done.samples;
                    expired.reclaimed += done.reclaimed;
                }
            }
            Ok((zstd, expired))
        })?;
        self.codecs.give_back(zstd);
        Ok(expired)
    }
}

/// Expires one series, or suspends it when its bytes no longer read or its
/// version cannot move; any other failure fails the whole write.
fn expire_isolated(tx: &Tx<'_>, zstd: &mut Zstd, series: &Due) -> Result<Option<Expired>> {
    execute(tx, SAVEPOINT, params![], "start a series' expiry")?;
    let expired = expire_series(tx, zstd, series).map_err(|error| error.within(format!("series {}", series.id)));
    let error = match expired {
        Ok(done) => {
            execute(tx, RELEASE, params![], "end a series' expiry")?;
            return Ok(Some(done));
        }
        Err(error) => error,
    };
    execute(tx, ROLLBACK_TO, params![], "undo a series' expiry")?;
    execute(tx, RELEASE, params![], "end a series' expiry")?;
    if !matches!(error.kind(), ErrorKind::Corrupt | ErrorKind::Limit) {
        return Err(error);
    }
    let failure: String = error.to_string().chars().take(1024).collect();
    let now = series.cutoff.saturating_add(series.keep);
    if execute(tx, SUSPEND, params![series.id, now, failure], "suspend a series")? == 1 {
        tracing::warn!(target: "tinystore", series = series.id, failure, "metrics: a series stopped taking samples");
    }
    Ok(None)
}

/// Removes one series' samples behind its cutoff, and the series itself once
/// nothing is left of it.
fn expire_series(tx: &Tx<'_>, zstd: &mut Zstd, due: &Due) -> Result<Expired> {
    let Some((version, max_seen, extent, bytes)) = read_head(tx, due.id)? else { return Ok(Expired::default()) };
    if version == i64::MAX {
        return Err(Error::limit("its version cannot move once more"));
    }
    let mut points = Vec::new();
    if let Some(bytes) = &bytes {
        let chunks = head::parse(due.id, extent, bytes)?;
        head::decode(zstd, &chunks, i64::MIN, i64::MAX, &mut points)?;
    }
    let kept_from = points.partition_point(|point| point.at < due.cutoff);
    let expired = Expired { samples: kept_from as u64, reclaimed: 0 };
    let points = &points[kept_from..];
    if kept_from > 0 {
        let extent = Extent::of(points);
        let encoded = head::encode(zstd, due.id, &[], points)?;
        if encoded.is_empty() {
            execute(tx, DROP_HEAD, params![due.id], "remove a head")?;
        } else {
            execute(tx, SAVE_HEAD, params![due.id, encoded], "write a head")?;
        }
        let saved = params![due.id, ready(extent, max_seen), extent.count as i64, first_of(&extent), last_of(&extent)];
        execute(tx, SAVE_STATE, saved, "write a series' state")?;
    }
    let expired = Expired { samples: expired.samples + expire_blocks(tx, zstd, due)?, ..expired };
    if points.is_empty() && !query_one::<bool>(tx, HAS_GROUPS, params![due.id], "look for a series' groups")? {
        reclaim(tx, due.id)?;
        return Ok(Expired { reclaimed: 1, ..expired });
    }
    let (block, head) = (publish::oldest_block(tx, zstd, due.id)?, points.first().map(|point| point.at));
    let next = earlier(block.map(|(_, end)| end), head).map(|oldest| expires(oldest, due.keep));
    let oldest = earlier(block.map(|(start, _)| start), head);
    let saved = params![due.id, next, expired.samples > 0, oldest];
    execute(tx, SET_DUE, saved, "write when a series is due").map(drop)?;
    Ok(expired)
}

/// Retires the blocks whose last sample has expired, the oldest groups'
/// first: a block goes whole, its payload with it, and a group once it holds
/// none, its clock let go; a group partly expired is rewritten with its live
/// mask, every payload keeping its address.
fn expire_blocks(tx: &Tx<'_>, zstd: &mut Zstd, due: &Due) -> Result<u64> {
    let mut expired = 0;
    while let Some((row_id, row)) = group_row(tx, OLDEST, due.id, params![due.id])? {
        let mut group = read_group(tx, zstd, &row)?;
        let mut changed = false;
        for slot in 0..group.blocks.len() {
            let block = &group.blocks[slot];
            if !group.is_live(slot) {
                continue;
            }
            if block.times.end >= due.cutoff {
                break;
            }
            if block.external() && execute(tx, DROP_PAYLOAD, params![block.payload], "remove a block's values")? != 1 {
                return Err(Error::corrupt("a block's payload is missing"));
            }
            expired += block.times.count as u64;
            group.live &= !(1 << slot);
            changed = true;
        }
        if group.live == 0 {
            execute(tx, DROP_GROUP, params![row_id], "remove a group")?;
            clock::let_go(tx, group.clock)?;
            continue;
        }
        if changed {
            let directory = group.encode(zstd)?;
            execute(tx, REWRITE_GROUP, params![row_id, directory], "rewrite a group's directory")?;
        }
        break;
    }
    Ok(expired)
}

/// When a sample at `at` of a name that keeps `keep` milliseconds has
/// expired: the first time it is older than the name keeps.
pub(super) fn expires(at: i64, keep: i64) -> i64 {
    at.saturating_add(keep).saturating_add(1)
}

fn first_of(extent: &Extent) -> Option<i64> {
    (extent.count > 0).then_some(extent.start)
}

fn last_of(extent: &Extent) -> Option<i64> {
    (extent.count > 0).then_some(extent.end)
}

type HeadRow = (i64, i64, Extent, Option<Vec<u8>>);

fn read_head(connection: &Connection, id: i64) -> Result<Option<HeadRow>> {
    let row = connection.prepare_cached(HEAD).and_then(|mut select| {
        select
            .query_row(params![id], |row| {
                let count: i64 = row.get(2)?;
                let extent = Extent {
                    count: usize::try_from(count).unwrap_or(0),
                    start: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    end: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                };
                Ok((row.get(0)?, row.get(1)?, extent, row.get(5)?))
            })
            .optional()
    });
    row.map_err(|error| sql_error("read a series' head", error))
}

const SERIES_ROW: &str = "select label_ids, name from series where id = ?1";
const UNCOUNT: &str = "update label_values set posting_count = posting_count - 1 where id = ?1";
const UNPOST: &str = "delete from postings where label_id = ?1 and series_id = ?2";
const PAIR_TEXT: &str = "select name, value from label_values where id = ?1";
/// A name's label its last series held goes; one that others hold is held
/// by one fewer.
const UNLABEL_LAST: &str = "delete from name_labels where name = ?1 and label = ?2 and value = ?3 and series = 1";
const UNLABEL: &str = "update name_labels set series = series - 1 where name = ?1 and label = ?2 and value = ?3";
/// A name loses its kind and its newest time with its last series, so that
/// its next may have another kind.
const FREE_KIND: &str = "update names set kind = null, newest = null where id = ?1
    and coalesce((select posting_count from label_values where name = '__name__' and value = names.name), 0) = 0";
const UNSTATE: &str = "delete from series_state where series_id = ?1";
const UNSERIES: &str = "delete from series where id = ?1";
const UNUSED_PAIR: &str = "delete from label_values where id = ?1 and posting_count = 0";
const ONE_SERIES_LESS: &str = "update store_state set series_count = series_count - 1 where id = 1";

/// Removes a series and what it holds: its postings and their counts, its
/// name's labels, the pairs no other series uses, its head, its state, its
/// row and its place among the store's series.
pub(super) fn reclaim(tx: &Tx<'_>, id: i64) -> Result<()> {
    // the series' pairs from its row, so that its postings go by their key:
    // the postings have no index by series, and a walk of them all would
    // grow with every series of the store
    let (encoded, name) = series_row(tx, id)?;
    let pairs = decode_ids(&encoded)?;
    for pair in &pairs {
        if execute(tx, UNPOST, params![pair, id], "remove a series' posting")? != 1 {
            return Err(Error::corrupt("a series' posting is missing"));
        }
        execute(tx, UNCOUNT, params![pair], "uncount a series' label")?;
        unlabel(tx, name, *pair)?;
    }
    execute(tx, FREE_KIND, params![name], "free a name's kind")?;
    execute(tx, DROP_HEAD, params![id], "remove a head")?;
    execute(tx, UNSTATE, params![id], "remove a series' state")?;
    execute(tx, UNSERIES, params![id], "remove a series")?;
    for pair in &pairs {
        execute(tx, UNUSED_PAIR, params![pair], "remove a pair no series uses")?;
    }
    execute(tx, ONE_SERIES_LESS, params![], "count a series less").map(drop)
}

/// A series' label ids and its name's id.
fn series_row(tx: &Tx<'_>, id: i64) -> Result<(Vec<u8>, i64)> {
    let row = tx
        .prepare_cached(SERIES_ROW)
        .and_then(|mut select| select.query_row(params![id], |row| Ok((row.get(0)?, row.get(1)?))).optional());
    row.map_err(|error| sql_error("read a series' row", error))?
        .ok_or_else(|| Error::corrupt("a series without its row"))
}

/// One series fewer holds a label of its name; the store's own were never
/// counted.
fn unlabel(tx: &Tx<'_>, name: i64, pair: i64) -> Result<()> {
    let text = tx.prepare_cached(PAIR_TEXT).and_then(|mut select| {
        select.query_row(params![pair], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).optional()
    });
    let text = text.map_err(|error| sql_error("read a series' label", error))?;
    let (label, value) = text.ok_or_else(|| Error::corrupt("a series' label missing from the dictionary"))?;
    if super::labels::is_own(&label) {
        return Ok(());
    }
    if execute(tx, UNLABEL_LAST, params![name, label, value], "remove a name's label")? == 0 {
        execute(tx, UNLABEL, params![name, label, value], "uncount a name's label")?;
    }
    Ok(())
}
