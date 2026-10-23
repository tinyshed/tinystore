//! Merging: a stream's small segments merge four of a size at a time, so
//! that an event is written again a few times rather than once a packing:
//! four whose events stay under one power of four make one of the next.
//!
//! ```text
//! events   30 40 35 38        → one of 143
//!          143 150 160 140    → one of 593
//! ```
//!
//! The segment of lowest id holds what they merge into, and the others' rows
//! stay as places naming it, with where their events begin among its, so a
//! reader that follows segments in the order they were packed finds each
//! event once. The merged blocks are written, the members' removed and their
//! rows made places in one commit, so a read finds each event once, before
//! and after.
//!
//! A read gives a stream's events of one time in the order their blocks were
//! written, and a merge writes its blocks after every block there is. So a
//! member that begins where the one before ends is taken only when its
//! blocks were written after that one's, and a member waits while a block
//! beside the run, written after one of its own, holds one of its times:
//! merged, its event would come before the member's.

use std::collections::BTreeMap;

use rusqlite::{Connection, params};

use super::block;
use super::engine::{Records, execute};
use super::event::Kept;
use super::head::Codec;
use super::pack::{HELD, Made, PATIENCE, SEGMENT_EVENTS, insert_blocks, insert_keys, made_of, rows_of_blocks};
use super::repair::Part;
use super::segment::Schema;
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// How many segments one merge joins.
const FAN_IN: usize = 4;
/// A segment under it holds few enough events to merge, and under a
/// segment's weight over `FAN_IN` weighs little enough: four of them fit a
/// segment.
const SMALL_EVENTS: usize = SEGMENT_EVENTS / FAN_IN;

/// A segment that holds its own events, few enough to merge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Small {
    id: i64,
    first: i64,
    last: i64,
    held: usize,
    weight: usize,
    /// The ids of its first and last blocks, by which a read gives the
    /// events of one time of two segments.
    blocks: (i64, i64),
}

/// Where a block stands: its id, its segment and the times it spans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Placed {
    id: i64,
    segment: i64,
    first: i64,
    last: i64,
}

impl Placed {
    /// Whether the two may hold events of one time.
    fn meets(&self, other: &Placed) -> bool {
        self.first <= other.last && other.first <= self.last
    }
}

/// A segment's row and the blocks a merge reads of it, in their order.
struct Bodies {
    id: i64,
    row: Vec<u8>,
    blocks: Vec<(Placed, Vec<u8>)>,
}

/// What became of a run.
enum Merged {
    Written,
    /// The members a block beside the run would pass, left for a later pass.
    Waits(Vec<i64>),
}

const SMALL: &str = "select id, first_at, last_at, held, weight,
    (select min(id) from blocks where segment = segments.id), (select max(id) from blocks where segment = segments.id)
    from segments where stream = ?1 and holder is null and held < ?2 and weight < ?3 order by id";
const MEMBER_ROW: &str = "select body from segments where id = ?1 and holder is null";
const MEMBER_BLOCKS: &str = "select id, first_at, last_at, body from blocks where segment = ?1 order by id";
/// The blocks of a stream's segments whose times meet `[?2, ?3]`, written
/// after the block `?4`.
const WRITTEN_AFTER: &str = "select blocks.id, blocks.segment, blocks.first_at, blocks.last_at
    from segments join blocks on blocks.segment = segments.id
    where segments.stream = ?1 and segments.holder is null and segments.last_at >= ?2 and segments.first_at <= ?3
    and blocks.id > ?4 and blocks.last_at >= ?2 and blocks.first_at <= ?3";
const BLOCK_BODY: &str = "select body from blocks where id = ?1";
/// A member's blocks with their filters, and what it held.
const DROP_MEMBER: [&str; 4] = [
    "delete from block_traces where block in (select id from blocks where segment = ?1)",
    "delete from block_filters where block in (select id from blocks where segment = ?1)",
    "delete from blocks where segment = ?1",
    "delete from segment_keys where segment = ?1",
];
/// A segment's places go to the holder, their events now beginning `?2` on.
const MOVE_PLACES: &str = "update segments set holder = ?1, start = start + ?2 where holder = ?3";
const MAKE_PLACE: &str = "update segments set holder = ?1, start = start + ?2, first_at = null,
    last_at = null, held = 0, weight = 0, body = x'' where id = ?3 and holder is null";
const REWRITE_HOLDER: &str = "update segments set first_at = ?2, last_at = ?3, start = start + ?4,
    held = ?5, weight = ?6, body = ?7 where id = ?1 and holder is null";

impl Records {
    /// Merges each of `streams` until no four of its small segments are of a
    /// size, and says how many segments merged into others. A stream whose
    /// merge fails is left for the next pass, and the others still merge.
    pub(super) fn merge(&self, streams: impl IntoIterator<Item = i64>) -> Result<u64> {
        let (mut merged, mut failed) = (0, None);
        for stream in streams {
            match self.merge_stream(stream) {
                Ok(count) => merged += count,
                Err(error) => {
                    failed.get_or_insert(error);
                }
            }
        }
        failed.map_or(Ok(merged), Err)
    }

    fn merge_stream(&self, stream: i64) -> Result<u64> {
        let (mut merged, mut waiting) = (0, Vec::new());
        loop {
            let small = self.small(stream)?.into_iter().filter(|small| !waiting.contains(&small.id)).collect();
            let Some(run) = pick(small) else { return Ok(merged) };
            match self.merge_run(stream, &run) {
                Ok(Merged::Written) => merged += run.len() as u64 - 1,
                Ok(Merged::Waits(members)) => waiting.extend(members),
                // a member met that no longer reads is left out of the next pick
                Err(_) if run.iter().any(|member| self.is_damaged(Part::Segment, member.id)) => {}
                Err(error) => return Err(error),
            }
        }
    }

    /// A stream's segments that hold their own events, under a quarter of
    /// what a segment holds, and read.
    fn small(&self, stream: i64) -> Result<Vec<Small>> {
        let failed = |error| sql_error("find the segments to merge", error);
        let small_weight = self.segment_weight() / FAN_IN;
        self.file.read(|connection| {
            let mut select = connection.prepare_cached(SMALL).map_err(failed)?;
            let found = select.query_map(params![stream, SMALL_EVENTS as i64, small_weight as i64], |row| {
                let (held, weight): (i64, i64) = (row.get(3)?, row.get(4)?);
                let (held, weight) = (held.max(0) as usize, weight.max(0) as usize);
                let blocks = (row.get::<_, Option<i64>>(5)?.unwrap_or(0), row.get::<_, Option<i64>>(6)?.unwrap_or(0));
                Ok(Small { id: row.get(0)?, first: row.get(1)?, last: row.get(2)?, held, weight, blocks })
            });
            let found: Vec<Small> = found.and_then(Iterator::collect).map_err(failed)?;
            Ok(found.into_iter().filter(|small| !self.is_damaged(Part::Segment, small.id)).collect())
        })
    }

    /// Merges a run of segments in the order of their times: read them and
    /// the blocks beside them in one snapshot → parse their events one after
    /// another → leave the members a block beside them would pass → make one
    /// segment of them → write it in place of them, in one commit.
    fn merge_run(&self, stream: i64, run: &[Small]) -> Result<Merged> {
        let weight = run.iter().map(|member| member.weight as u64).sum::<u64>();
        let _held = self.memory.reserve(weight * HELD, "records: merging segments", PATIENCE)?;
        let (members, beside) = self.members(stream, run)?;
        let _beside = self.memory.reserve(bytes_of(&beside), "records: merging segments", PATIENCE)?;
        let mut codec = Codec::default();
        let (mut events, mut times): (Vec<Kept>, Vec<(Placed, Vec<i64>)>) = (Vec::new(), Vec::new());
        let name = self.name_of(stream);
        for (member, read) in run.iter().zip(&members) {
            let within = |error: Error| self.met((Part::Segment, member.id), &name, error);
            let schema = Schema::parse(&read.row, &mut codec).map_err(within)?;
            for (placed, body) in &read.blocks {
                let parsed = block::parse(&schema, body, &mut codec).map_err(within)?;
                times.push((*placed, parsed.iter().map(|event| event.at).collect()));
                events.extend(parsed);
            }
        }
        let waits = self.passed(&name, &times, &beside, &mut codec);
        if !waits.is_empty() {
            return Ok(Merged::Waits(waits));
        }
        let made = made_of(&events, &mut codec)?;
        self.publish_merge(stream, run, &made).map(|()| Merged::Written)
    }

    /// A run's members and the segments beside it, read in one snapshot.
    /// Only a maintenance pass writes blocks, one pass at a time, so none is
    /// written between this read and the merge's commit.
    fn members(&self, stream: i64, run: &[Small]) -> Result<(Vec<Bodies>, Vec<Bodies>)> {
        let failed = |error| sql_error("read the segments to merge", error);
        self.file.read(|connection| {
            let mut row = connection.prepare_cached(MEMBER_ROW).map_err(failed)?;
            let mut blocks = connection.prepare_cached(MEMBER_BLOCKS).map_err(failed)?;
            let mut members = Vec::with_capacity(run.len());
            for member in run {
                let body: Vec<u8> = row.query_row([member.id], |row| row.get(0)).map_err(failed)?;
                let found = blocks.query_map([member.id], |row| {
                    let placed = Placed { id: row.get(0)?, segment: member.id, first: row.get(1)?, last: row.get(2)? };
                    Ok((placed, row.get(3)?))
                });
                members.push(Bodies {
                    id: member.id,
                    row: body,
                    blocks: found.and_then(Iterator::collect).map_err(failed)?,
                });
            }
            let beside = beside(connection, stream, run, &members).map_err(failed)?;
            Ok((members, beside))
        })
    }

    /// The members whose events of a time a block beside the run would come
    /// before once it is merged, since the merged blocks are written after
    /// it. A block beside it that no longer reads is noted as damage, and
    /// the members whose blocks it meets wait: nothing shows they share no
    /// time with it.
    fn passed(&self, name: &str, mine: &[(Placed, Vec<i64>)], beside: &[Bodies], codec: &mut Codec) -> Vec<i64> {
        let mut waits = Vec::new();
        for segment in beside {
            let noted = |error: Error| self.met((Part::Segment, segment.id), name, error);
            let schema = Schema::parse(&segment.row, codec).map_err(noted).ok();
            for (placed, body) in &segment.blocks {
                let times = schema.as_ref().and_then(|schema| block::times(schema, body).map_err(noted).ok());
                for (member, member_times) in mine {
                    let shares = |times: &Vec<i64>| times.iter().any(|at| member_times.binary_search(at).is_ok());
                    let passes = member.id < placed.id && member.meets(placed) && times.as_ref().is_none_or(shares);
                    if passes && !waits.contains(&member.segment) {
                        waits.push(member.segment);
                    }
                }
            }
        }
        waits
    }

    /// Writes a merged segment in its holder's row, which keeps its place,
    /// removes its members' blocks, and makes every other member a place the
    /// holder holds, with the places it held: all or nothing, a member that
    /// changed meanwhile failing the whole.
    fn publish_merge(&self, stream: i64, run: &[Small], made: &Made) -> Result<()> {
        let holder = run.iter().map(|member| member.id).min().unwrap_or_default();
        // where each member's events begin among the merged, the run being in time order
        let begins: Vec<(i64, i64)> = run
            .iter()
            .scan(0, |begin, member| {
                let at = *begin;
                *begin += member.held as i64;
                Some((member.id, at))
            })
            .collect();
        let holder_begins = begins.iter().find(|(id, _)| *id == holder).map_or(0, |(_, begin)| *begin);
        let bytes = made.blocks.iter().map(|block| block.body.len()).sum();
        let (first, last, events, weight) = (made.first, made.last, made.events as i64, made.weight as i64);
        let (shared, keys, blocks) = (made.shared.clone(), made.keys.clone(), rows_of_blocks(made));
        self.file.write(bytes, move |tx| {
            let changed = |id: i64| Error::internal(format!("records: segment {id} changed while it was merged"));
            for (member, sql) in begins.iter().flat_map(|(member, _)| DROP_MEMBER.map(|sql| (member, sql))) {
                execute(tx, sql, [member], "remove what a merged segment held")?;
            }
            insert_blocks(tx, holder, stream, &blocks)?;
            insert_keys(tx, holder, &keys)?;
            // the holder's own places first, so that none moves twice
            execute(tx, MOVE_PLACES, params![holder, holder_begins, holder], "move a merged segment's places")?;
            for (member, begin) in begins.iter().filter(|(member, _)| *member != holder) {
                execute(tx, MOVE_PLACES, params![holder, begin, member], "move a merged segment's places")?;
                if execute(tx, MAKE_PLACE, params![holder, begin, member], "make a merged segment a place")? != 1 {
                    return Err(changed(*member));
                }
            }
            let rewritten = params![holder, first, last, holder_begins, events, weight, shared];
            if execute(tx, REWRITE_HOLDER, rewritten, "write a merged segment")? != 1 {
                return Err(changed(holder));
            }
            Ok(())
        })
    }
}

/// The segments of `stream` beside `run`, each with its blocks written after
/// a member's block whose times they meet.
fn beside(connection: &Connection, stream: i64, run: &[Small], members: &[Bodies]) -> rusqlite::Result<Vec<Bodies>> {
    let mine: Vec<Placed> = members.iter().flat_map(|member| member.blocks.iter().map(|(placed, _)| *placed)).collect();
    let span = (mine.iter().map(|block| block.first).min(), mine.iter().map(|block| block.last).max());
    let (Some(first), Some(last), Some(oldest)) = (span.0, span.1, mine.iter().map(|block| block.id).min()) else {
        return Ok(Vec::new());
    };
    let mut select = connection.prepare_cached(WRITTEN_AFTER)?;
    let found = select.query_map(params![stream, first, last, oldest], |row| {
        Ok(Placed { id: row.get(0)?, segment: row.get(1)?, first: row.get(2)?, last: row.get(3)? })
    })?;
    let mut after: BTreeMap<i64, Vec<Placed>> = BTreeMap::new();
    for placed in found {
        let placed = placed?;
        let passes = |member: &Placed| member.id < placed.id && member.meets(&placed);
        if !run.iter().any(|member| member.id == placed.segment) && mine.iter().any(passes) {
            after.entry(placed.segment).or_default().push(placed);
        }
    }
    let (mut row, mut body) = (connection.prepare_cached(MEMBER_ROW)?, connection.prepare_cached(BLOCK_BODY)?);
    let mut beside = Vec::with_capacity(after.len());
    for (segment, placed) in after {
        let blocks = placed.into_iter().map(|placed| Ok((placed, body.query_row([placed.id], |row| row.get(0))?)));
        let blocks = blocks.collect::<rusqlite::Result<_>>()?;
        beside.push(Bodies { id: segment, row: row.query_row([segment], |row| row.get(0))?, blocks });
    }
    Ok(beside)
}

/// The bytes a merge read of the segments beside its run.
fn bytes_of(beside: &[Bodies]) -> u64 {
    let blocks = |segment: &Bodies| segment.blocks.iter().map(|(_, body)| body.len()).sum::<usize>();
    beside.iter().map(|segment| segment.row.len() + blocks(segment)).sum::<usize>() as u64
}

/// The next segments to merge, the smallest size first: four of one size,
/// taken in the order of their times.
fn pick(small: Vec<Small>) -> Option<Vec<Small>> {
    let mut by_size: BTreeMap<u32, Vec<Small>> = BTreeMap::new();
    for segment in small {
        by_size.entry(size_of(segment.held)).or_default().push(segment);
    }
    by_size.into_values().map(in_time_order).find(|run| run.len() == FAN_IN)
}

/// The power of four a segment's events stay under:
///
/// ```text
/// 1…3 → 1      4…15 → 2      16…63 → 3      143 → 4      593 → 5
/// ```
fn size_of(events: usize) -> u32 {
    (usize::BITS - events.leading_zeros()).div_ceil(2)
}

/// Up to four segments, earliest first, each beginning where the one before
/// ended or later, so that their events one after another are in the order
/// of their times; one that begins before the last taken ends is left out.
/// So is one that begins where it ends but whose blocks were written before
/// its: a read gives the events of one time of two blocks in the order the
/// blocks were written, and the merge would give them the other way round.
fn in_time_order(mut segments: Vec<Small>) -> Vec<Small> {
    segments.sort_by_key(|segment| (segment.first, segment.id));
    let mut run: Vec<Small> = Vec::with_capacity(FAN_IN);
    for segment in segments {
        let after = |last: &Small| {
            segment.first > last.last || (segment.first == last.last && segment.blocks.0 > last.blocks.1)
        };
        if run.last().is_some_and(|last| !after(last)) {
            continue;
        }
        run.push(segment);
        if run.len() == FAN_IN {
            break;
        }
    }
    run
}

#[cfg(test)]
#[path = "merge_tests.rs"]
mod tests;
