//! Publication: a series' new blocks written in one savepoint of the writer,
//! with everything they change, or nothing.
//!
//! ```text
//! the version it was read at ─► the groups before it merged in ─► its clock held ─► payload ids given
//!   ─► the merged groups replaced by one ─► its samples taken out of the head ─► what is sealed moved
//! ```
//!
//! A group takes in the group before it while that one has no more live
//! blocks than it, so that a group at least doubles each time it is rewritten
//! and a block's directory entry is rewritten a few times at most. Payloads
//! keep their ids, so that a merge rewrites directories and no body.

use rusqlite::{Connection, OptionalExtension, params};

use super::clock::{self, MOST_BLOCKS, Times};
use super::codec::Point;
use super::directory::{Group, MOST_ENTRIES, Row};
use super::engine::{Engine, execute, query_one};
use super::head::{self, Extent};
use super::ingest::ready;
use super::pack::{Packed, Staged, mask};
use super::retention::expires;
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
const INSERT_GROUP: &str =
    "insert into groups (series_id, start_ts, end_ts, clock, directory) values (?1, ?2, ?3, ?4, ?5)";
const HEAD: &str = "select st.max_seen, st.head_count, st.head_start, st.head_end, h.body
    from series_state st left join heads h on h.series_id = st.series_id where st.series_id = ?1";
const SAVE_HEAD: &str = "update heads set body = ?2 where series_id = ?1";
const DROP_HEAD: &str = "delete from heads where series_id = ?1";
const SEAL: &str = "update series_state set sealed_before = ?2, version = version + 1, ready = ?3, scale = ?4,
    head_count = ?5, head_start = ?6, head_end = ?7, due = ?8 where series_id = ?1";
const SUSPEND: &str = "update series_state set failed_at = ?2, failure = ?3 where series_id = ?1 and failed_at is null";

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
        Ok(())
    }
}

fn publish_isolated(tx: &Tx<'_>, zstd: &mut Zstd, series: &Staged, now: i64, done: &mut Packed) -> Result<()> {
    execute(tx, SAVEPOINT, params![], "start a series' publication")?;
    let published = publish_series(tx, zstd, series);
    if published.is_ok() {
        execute(tx, RELEASE, params![], "end a series' publication")?;
        done.samples += series.candidate.points.len() as u64;
        done.blocks += series.group.blocks.len() as u64;
        return Ok(());
    }
    execute(tx, ROLLBACK_TO, params![], "undo a series' publication")?;
    execute(tx, RELEASE, params![], "end a series' publication")?;
    let error = published.err().unwrap_or_else(|| Error::internal("a publication"));
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

/// Writes one series' blocks and everything they change.
fn publish_series(tx: &Tx<'_>, zstd: &mut Zstd, series: &Staged) -> Result<()> {
    let candidate = &series.candidate;
    let version: i64 = query_one(tx, VERSION, params![candidate.id], "read a series' version")?;
    if version != candidate.version {
        return Err(Error::new(ErrorKind::Conflict, "a series written to since packing read it"));
    }
    if version == i64::MAX {
        return Err(Error::limit("its version cannot move once more"));
    }
    let (mut group, merged) = merge_before(tx, zstd, series.group.clone())?;
    group.clock = clock::hold(tx, &clock::encode(&times_of(&group)))?;
    give_payloads(tx, &mut group)?;
    for (row, clock) in merged {
        execute(tx, DROP_GROUP, params![row], "remove a group merged into the next")?;
        clock::let_go(tx, clock)?;
    }
    let directory = group.encode(zstd)?;
    let row = params![group.series, group.start, group.end, group.clock, directory];
    execute(tx, INSERT_GROUP, row, "write a group")?;
    seal(tx, zstd, series)
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
/// sealed past them: one sample that changed since packing read it means a
/// write won the race.
fn seal(tx: &Tx<'_>, zstd: &mut Zstd, series: &Staged) -> Result<()> {
    let candidate = &series.candidate;
    let (max_seen, extent, bytes) = head_row(tx, candidate.id)?;
    let mut points = Vec::new();
    if let Some(bytes) = &bytes {
        head::decode(zstd, &head::parse(candidate.id, extent, bytes)?, i64::MIN, i64::MAX, &mut points)?;
    }
    let left = without(&points, &candidate.points)?;
    let encoded = head::encode(zstd, candidate.id, &[], &left)?;
    if encoded.is_empty() {
        execute(tx, DROP_HEAD, params![candidate.id], "remove a head")?;
    } else {
        execute(tx, SAVE_HEAD, params![candidate.id, encoded], "write a head")?;
    }
    let extent = Extent::of(&left);
    let sealed_before = series.group.end.saturating_add(1);
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
    let Some((_, oldest)) = group_row(connection, OLDEST, id, params![id])? else { return Ok(head_start) };
    let group = read_group(connection, zstd, &oldest)?;
    let first_live = (0..group.blocks.len()).find(|slot| group.is_live(*slot)).map(|slot| group.blocks[slot].times.end);
    Ok(match (first_live, head_start) {
        (Some(block), Some(head)) => Some(block.min(head)),
        (block, head) => block.or(head),
    })
}
