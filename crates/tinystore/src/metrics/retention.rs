//! What names keep no longer: each series is due at its oldest sample plus
//! its name's `keep`, one indexed value a series, and a pass takes a bounded
//! batch of what is due. A head's expired samples go at once; a series left
//! with nothing goes with its postings and the dictionary pairs no other
//! series uses, and frees its place among the store's series.

use rusqlite::{Connection, OptionalExtension, params};

use super::engine::{Engine, execute, query_one};
use super::head::{self, Extent};
use super::ingest::ready;
use super::names::{KEEP, millis};
use crate::compress::frame::Zstd;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, Result};

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
const SAVE_HEAD: &str = "update heads set body = ?2 where series_id = ?1";
const DROP_HEAD: &str = "delete from heads where series_id = ?1";
const SAVE_STATE: &str = "update series_state set version = version + 1, ready = ?2,
    head_count = ?3, head_start = ?4, head_end = ?5 where series_id = ?1";
const SET_DUE: &str = "update series_state set due = ?2 where series_id = ?1";
const HAS_GROUPS: &str = "select exists (select 1 from groups where series_id = ?1)";

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

    /// Removes what each series due keeps no longer, in one write.
    pub(super) fn expire(&self, due: Vec<Due>) -> Result<Expired> {
        if due.is_empty() {
            return Ok(Expired::default());
        }
        let zstd = self.codecs.take();
        let (zstd, expired) = self.file.write(0, move |tx| {
            let mut zstd = zstd;
            let mut expired = Expired::default();
            for series in &due {
                let done = expire_series(tx, &mut zstd, series)
                    .map_err(|error| error.within(format!("series {}", series.id)))?;
                expired.samples += done.samples;
                expired.reclaimed += done.reclaimed;
            }
            Ok((zstd, expired))
        })?;
        self.codecs.give_back(zstd);
        Ok(expired)
    }
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
    if points.is_empty() && !query_one::<bool>(tx, HAS_GROUPS, params![due.id], "look for a series' groups")? {
        reclaim(tx, due.id)?;
        return Ok(Expired { reclaimed: 1, ..expired });
    }
    let next = points.first().map(|point| expires(point.at, due.keep));
    execute(tx, SET_DUE, params![due.id, next], "write when a series is due").map(drop)?;
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

const POSTED: &str = "select label_id from postings where series_id = ?1";
const UNCOUNT: &str = "update label_values set posting_count = posting_count - 1
    where id in (select label_id from postings where series_id = ?1)";
/// A name loses its kind with its last series, so that its next may have
/// another.
const FREE_KIND: &str = "update names set kind = null where id = (select name from series where id = ?1)
    and coalesce((select posting_count from label_values where name = '__name__' and value = names.name), 0) = 0";
const UNPOST: &str = "delete from postings where series_id = ?1";
const UNSTATE: &str = "delete from series_state where series_id = ?1";
const UNSERIES: &str = "delete from series where id = ?1";
const UNUSED_PAIRS: &str =
    "delete from label_values where posting_count = 0 and id in (select value from json_each(?1))";
const ONE_SERIES_LESS: &str = "update store_state set series_count = series_count - 1 where id = 1";

/// Removes an empty series: its postings and their counts, the pairs no
/// other series uses, its state, its row and its place among the store's
/// series.
pub(super) fn reclaim(tx: &Tx<'_>, id: i64) -> Result<()> {
    let failed = |error| sql_error("read a series' postings", error);
    let pairs: Vec<i64> = tx
        .prepare_cached(POSTED)
        .and_then(|mut select| select.query_map(params![id], |row| row.get(0)).and_then(Iterator::collect))
        .map_err(failed)?;
    let pairs = serde_json::to_string(&pairs).map_err(|error| Error::internal("a series' pairs").with_source(error))?;
    execute(tx, UNCOUNT, params![id], "uncount a series' labels")?;
    execute(tx, FREE_KIND, params![id], "free a name's kind")?;
    execute(tx, UNPOST, params![id], "remove a series' postings")?;
    execute(tx, DROP_HEAD, params![id], "remove a head")?;
    execute(tx, UNSTATE, params![id], "remove a series' state")?;
    execute(tx, UNSERIES, params![id], "remove a series")?;
    execute(tx, UNUSED_PAIRS, params![pairs], "remove the pairs no series uses")?;
    execute(tx, ONE_SERIES_LESS, params![], "count a series less").map(drop)
}
