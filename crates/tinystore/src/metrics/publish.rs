//! Publication: a series' new blocks written in one savepoint of the writer,
//! with everything they change, or nothing.
//!
//! ```text
//! what its name keeps no longer expired ─► the groups before it merged in ─► its clock held ─► payload ids given
//!   ─► the merged groups replaced by one ─► its samples taken out of the head, none written among them
//!   ─► what is sealed moved
//! ```
//!
//! A group takes in the group before it while that one has no more live
//! blocks than it, so that a group at least doubles each time it is rewritten
//! and a block's directory entry is rewritten a few times at most. Payloads
//! keep their ids, so that a merge rewrites directories and no body.
//!
//! A group of the most blocks is never taken in again: its row and its clock
//! go after every row of their tables, so that the pages they fill stay full,
//! and a group still growing goes below zero, among the rows merges remove.
//! Written among them, a third of either table's pages was empty, Pingmon's
//! a second.

use rusqlite::{Connection, OptionalExtension, params};

use super::clock::{self, MOST_BLOCKS, Times};
use super::codec::Point;
use super::directory::{Group, MOST_ENTRIES, Row};
use super::engine::{Engine, execute, query_one};
use super::head::{self, Extent};
use super::ingest::ready;
use super::pack::{Candidate, Packed, Staged, mask};
use super::retention::{Due, expire_series, expires};
use crate::compress::frame::Zstd;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, ErrorKind, Result};

const SAVEPOINT: &str = "savepoint metrics_series";
const RELEASE: &str = "release metrics_series";
const ROLLBACK_TO: &str = "rollback to metrics_series";
const VERSION: &str = "select version from series_state where series_id = ?1";
const BEFORE: &str = "select id, start_ts, end_ts, clock, directory from groups
    where series_id = ?1 and start_ts < ?2 order by start_ts desc limit 1";
const OLDEST: &str = "select id, start_ts, end_ts, clock, directory from groups
    where series_id = ?1 order by start_ts limit 1";
const CLOCK: &str = "select body from clocks where id = ?1";
const NEXT_PAYLOAD: &str = "select next_payload_id from store_state where id = 1";
const TAKE_PAYLOADS: &str = "update store_state set next_payload_id = ?1 where id = 1";
const INSERT_PAYLOAD: &str = "insert into payloads (id, body) values (?1, ?2)";
const DROP_GROUP: &str = "delete from groups where id = ?1";
const INSERT_LASTING_GROUP: &str = "insert into groups (id, series_id, start_ts, end_ts, clock, directory)
    values ((select max(coalesce(max(id), 0), 0) + 1 from groups), ?1, ?2, ?3, ?4, ?5)";
const INSERT_GROWING_GROUP: &str = "insert into groups (id, series_id, start_ts, end_ts, clock, directory)
    values ((select min(coalesce(min(id), 0), 0) - 1 from groups), ?1, ?2, ?3, ?4, ?5)";
const HEAD: &str = "select st.max_seen, st.head_count, st.head_start, st.head_end, h.body
    from series_state st left join heads h on h.series_id = st.series_id where st.series_id = ?1";
const DROP_HEAD: &str = "delete from heads where series_id = ?1";
const INSERT_HEAD: &str = "insert into heads (series_id, body) values (?1, ?2)";
#[cfg(test)]
const SAVE_HEAD: &str = "update heads set body = ?2 where series_id = ?1";

#[cfg(test)]
thread_local! {
    /// A head packing or expiry shrank is rewritten in place on this thread,
    /// as 0.1.0 did, for the measurement that compares the two.
    pub(super) static IN_PLACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
const SEAL: &str = "update series_state set sealed_before = ?2, version = version + 1, ready = ?3, scale = ?4,
    head_count = ?5, head_start = ?6, head_end = ?7, due = ?8 where series_id = ?1";
pub(super) const SUSPEND: &str =
    "update series_state set failed_at = ?2, failure = ?3 where series_id = ?1 and failed_at is null";

impl Engine {
    /// Publishes staged series in one write, each in a savepoint, so that a
    /// series another write moved since it was read, or whose bytes no longer
    /// read, undoes only its own changes and the rest go on.
    pub(super) fn publish(&self, staged: Vec<Staged>, packed: &mut Packed) -> Result<()> {
        if staged.is_empty() {
            return Ok(());
        }
        let now = self.now();
        let bytes = staged.iter().map(|series| series.candidate.points.len() * 16).sum();
        let zstd = self.codecs.take();
        let (zstd, done) = self.file.write(bytes, move |tx| {
            let mut zstd = zstd;
            let mut done = Packed::default();
            for series in &staged {
                publish_isolated(tx, &mut zstd, series, now, &mut done)?;
            }
            Ok((zstd, done))
        })?;
        self.codecs.give_back(zstd);
        packed.samples += done.samples;
        packed.blocks += done.blocks;
        packed.conflicts += done.conflicts;
        packed.suspended += done.suspended;
        packed.expired += done.expired;
        Ok(())
    }
}

fn publish_isolated(tx: &Tx<'_>, zstd: &mut Zstd, series: &Staged, now: i64, done: &mut Packed) -> Result<()> {
    execute(tx, SAVEPOINT, params![], "start a series' publication")?;
    let error = match publish_series(tx, zstd, series) {
        Ok(expired) => {
            execute(tx, RELEASE, params![], "end a series' publication")?;
            done.samples += series.candidate.points.len() as u64;
            done.blocks += series.group.blocks.len() as u64;
            done.expired += expired;
            return Ok(());
        }
        Err(error) => error,
    };
    execute(tx, ROLLBACK_TO, params![], "undo a series' publication")?;
    execute(tx, RELEASE, params![], "end a series' publication")?;
    match error.kind() {
        ErrorKind::Conflict => done.conflicts += 1,
        ErrorKind::Corrupt | ErrorKind::Limit => {
            let failure: String = error.to_string().chars().take(1024).collect();
            let id = series.candidate.id;
            if execute(tx, SUSPEND, params![id, now, failure], "suspend a series")? == 1 {
                tracing::warn!(target: "tinystore", series = id, failure, "metrics: a series stopped taking samples");
                done.suspended += 1;
            }
        }
        _ => return Err(error),
    }
    Ok(())
}

/// Writes one series' blocks and everything they change, and says how many
/// samples it removed that its name kept no longer. A write since packing
/// read the series voids the publication only where it reached the samples
/// packed, which `seal` checks, so that a series written to without a pause
/// still packs; one whose head packing must expire first is published only
/// as packing read it, since that expiry goes by the head packing read.
fn publish_series(tx: &Tx<'_>, zstd: &mut Zstd, series: &Staged) -> Result<u64> {
    let candidate = &series.candidate;
    let version: i64 = query_one(tx, VERSION, params![candidate.id], "read a series' version")?;
    if version != candidate.version && candidate.behind > 0 {
        return Err(Error::new(ErrorKind::Conflict, "a series written to since packing read it"));
    }
    if version == i64::MAX {
        return Err(Error::limit("its version cannot move once more"));
    }
    let expired = expire_behind(tx, zstd, candidate)?;
    let (mut group, merged) = merge_before(tx, zstd, series.group.clone())?;
    let lasting = group.blocks.len() == MOST_BLOCKS;
    group.clock = clock::hold(tx, &clock::encode(&times_of(&group)), lasting)?;
    give_payloads(tx, &mut group)?;
    for (row, clock) in merged {
        execute(tx, DROP_GROUP, params![row], "remove a group merged into the next")?;
        clock::let_go(tx, clock)?;
    }
    let directory = group.encode(zstd)?;
    let row = params![group.series, group.start, group.end, group.clock, directory];
    let insert = if lasting { INSERT_LASTING_GROUP } else { INSERT_GROWING_GROUP };
    execute(tx, insert, row, "write a group")?;
    seal(tx, zstd, series)?;
    Ok(expired)
}

/// Expires the samples the head held before its cutoff, which packing left
/// out of the blocks: left in the head, they would lie before the blocks,
/// and a read that met them out of order would suspend the series. A pass
/// expires and packs series it chooses apart, so one ready may not have
/// been expired yet.
fn expire_behind(tx: &Tx<'_>, zstd: &mut Zstd, candidate: &Candidate) -> Result<u64> {
    if candidate.behind == 0 {
        return Ok(0);
    }
    let due = Due { id: candidate.id, cutoff: candidate.cutoff, keep: candidate.keep };
    Ok(expire_series(tx, zstd, &due)?.samples)
}

fn times_of(group: &Group) -> Vec<Times> {
    group.blocks.iter().map(|block| block.times.clone()).collect()
}

/// The group with the groups before it it takes in, each while that one has
/// no more live blocks than it and they fit one group, and those groups' rows
/// and clocks.
fn merge_before(tx: &Tx<'_>, zstd: &mut Zstd, mut group: Group) -> Result<(Group, Vec<(i64, i64)>)> {
    let mut merged = Vec::new();
    while group.blocks.len() < MOST_BLOCKS {
        let Some((row_id, before)) = group_row(tx, BEFORE, group.series, params![group.series, group.start])? else {
            break;
        };
        let before = read_group(tx, zstd, &before)?;
        let live: Vec<_> = before
            .blocks
            .iter()
            .enumerate()
            .filter(|(slot, _)| before.is_live(*slot))
            .map(|(_, block)| block.clone())
            .collect();
        if live.len() > group.blocks.len() || live.len() + group.blocks.len() > MOST_BLOCKS {
            break;
        }
        if before.end >= group.start {
            return Err(Error::corrupt("two groups of a series overlap"));
        }
        let mut blocks = live;
        blocks.extend(group.blocks.iter().cloned());
        let next = Group { start: blocks[0].times.start, live: mask(blocks.len()), blocks, ..group.clone() };
        if clock::encode(&times_of(&next)).len() > clock::MOST_BYTES || next.entries().len() > MOST_ENTRIES {
            break;
        }
        merged.push((row_id, before.clock));
        group = next;
    }
    Ok((group, merged))
}

/// A group's row of `series` found by `sql`, and its id.
pub(super) fn group_row(
    connection: &Connection,
    sql: &str,
    series: i64,
    params: impl rusqlite::Params,
) -> Result<Option<(i64, Row)>> {
    let row = connection.prepare_cached(sql).and_then(|mut select| {
        select
            .query_row(params, |row| {
                let found =
                    Row { series, start: row.get(1)?, end: row.get(2)?, clock: row.get(3)?, directory: row.get(4)? };
                Ok((row.get::<_, i64>(0)?, found))
            })
            .optional()
    });
    row.map_err(|error| sql_error("read a group", error))
}

/// A group of the writer's, its directory checked against its clock.
pub(super) fn read_group(tx: &Connection, zstd: &mut Zstd, row: &Row) -> Result<Group> {
    let body: Vec<u8> = query_one(tx, CLOCK, params![row.clock], "read a group's clock")?;
    Group::decode(zstd, row, clock::decode(&body)?)
}

/// Gives each new block that keeps its body apart a payload id, never given
/// before, and writes its body.
fn give_payloads(tx: &Tx<'_>, group: &mut Group) -> Result<()> {
    let wanted = group.blocks.iter().filter(|block| block.external() && block.payload == 0).count() as i64;
    if wanted == 0 {
        return Ok(());
    }
    let next: i64 = query_one(tx, NEXT_PAYLOAD, params![], "read the next payload's id")?;
    if next > i64::MAX - wanted {
        return Err(Error::limit("payload ids run out"));
    }
    execute(tx, TAKE_PAYLOADS, params![next + wanted], "take payload ids")?;
    let fresh = group.blocks.iter_mut().filter(|block| block.external() && block.payload == 0);
    for (id, block) in (next..).zip(fresh) {
        block.payload = id;
        execute(tx, INSERT_PAYLOAD, params![id, block.body], "write a block's values")?;
    }
    Ok(())
}

/// Takes exactly the samples packed out of the head, and moves what is
/// sealed past them: one sample that changed since packing read it, or one
/// written among them, means a write won the race.
fn seal(tx: &Tx<'_>, zstd: &mut Zstd, series: &Staged) -> Result<()> {
    let candidate = &series.candidate;
    let (max_seen, extent, bytes) = head_row(tx, candidate.id)?;
    let mut points = Vec::new();
    if let Some(bytes) = &bytes {
        head::decode(zstd, &head::parse(candidate.id, extent, bytes)?, i64::MIN, i64::MAX, &mut points)?;
    }
    let left = without(&points, &candidate.points)?;
    let sealed_before = series.group.end.saturating_add(1);
    if left.first().is_some_and(|point| point.at < sealed_before) {
        return Err(Error::new(ErrorKind::Conflict, "a sample written among those packed since packing read it"));
    }
    shrink_head(tx, candidate.id, head::encode(zstd, candidate.id, &[], &left)?)?;
    let extent = Extent::of(&left);
    let due =
        due(tx, zstd, candidate.id, left.first().map(|point| point.at))?.map(|oldest| expires(oldest, candidate.keep));
    let (start, end) = if left.is_empty() { (None, None) } else { (Some(extent.start), Some(extent.end)) };
    let sealed = params![
        candidate.id,
        sealed_before,
        ready(extent, max_seen),
        series.scale,
        extent.count as i64,
        start,
        end,
        due
    ];
    execute(tx, SEAL, sealed, "move what a series has sealed").map(drop)
}

/// Writes a head that packing or expiry left smaller, `encoded`, or removes
/// it when it holds nothing: out and in again rather than in place. SQLite
/// merges a page left less than a third full only when a row leaves it, so
/// heads rewritten smaller in place keep the pages of their largest for
/// good: after the bulk load of `what_a_bulk_load_leaves_in_its_file`,
/// 2.1 MiB of pages held 0.2 MiB of heads, and 0.2 MiB once they went out
/// and in.
pub(super) fn shrink_head(tx: &Tx<'_>, series: i64, encoded: Vec<u8>) -> Result<()> {
    #[cfg(test)]
    if IN_PLACE.get() && !encoded.is_empty() {
        return execute(tx, SAVE_HEAD, params![series, encoded], "write a head").map(drop);
    }
    execute(tx, DROP_HEAD, params![series], "remove a head")?;
    if encoded.is_empty() {
        return Ok(());
    }
    execute(tx, INSERT_HEAD, params![series, encoded], "write a head").map(drop)
}

type HeadRow = (i64, Extent, Option<Vec<u8>>);

fn head_row(connection: &Connection, id: i64) -> Result<HeadRow> {
    let row = connection.prepare_cached(HEAD).and_then(|mut select| {
        select.query_row(params![id], |row| {
            let count: i64 = row.get(1)?;
            let extent = Extent {
                count: usize::try_from(count).unwrap_or(0),
                start: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                end: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
            };
            Ok((row.get(0)?, extent, row.get(4)?))
        })
    });
    row.map_err(|error| sql_error("read a series' head", error))
}

/// The head's samples without those packed, each of which must still be
/// there, bit for bit.
fn without(points: &[Point], packed: &[Point]) -> Result<Vec<Point>> {
    let mut packed = packed.iter().peekable();
    let mut left = Vec::with_capacity(points.len());
    for point in points {
        match packed.peek() {
            Some(sealed) if sealed.at == point.at => {
                if sealed.value.to_bits() != point.value.to_bits() {
                    return Err(Error::new(ErrorKind::Conflict, "a sample changed since packing read it"));
                }
                packed.next();
            }
            _ => left.push(*point),
        }
    }
    if packed.next().is_some() {
        return Err(Error::new(ErrorKind::Conflict, "a sample gone since packing read it"));
    }
    Ok(left)
}

/// The time of a series' oldest sample that still counts towards its
/// expiry: its oldest live block's end, since a block goes whole, or its
/// head's first sample, whichever is earlier.
pub(super) fn due(connection: &Connection, zstd: &mut Zstd, id: i64, head_start: Option<i64>) -> Result<Option<i64>> {
    let block = oldest_block(connection, zstd, id)?;
    Ok(earlier(block.map(|(_, end)| end), head_start))
}

/// The first and last times of a series' oldest live block.
pub(super) fn oldest_block(connection: &Connection, zstd: &mut Zstd, id: i64) -> Result<Option<(i64, i64)>> {
    let Some((_, oldest)) = group_row(connection, OLDEST, id, params![id])? else { return Ok(None) };
    let group = read_group(connection, zstd, &oldest)?;
    let first_live = (0..group.blocks.len()).find(|slot| group.is_live(*slot));
    Ok(first_live.map(|slot| (group.blocks[slot].times.start, group.blocks[slot].times.end)))
}

/// The earlier of two times either may lack.
pub(super) fn earlier(one: Option<i64>, other: Option<i64>) -> Option<i64> {
    match (one, other) {
        (Some(one), Some(other)) => Some(one.min(other)),
        (one, other) => one.or(other),
    }
}
