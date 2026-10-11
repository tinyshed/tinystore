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

use std::collections::BTreeMap;

use rusqlite::params;

use super::block;
use super::engine::{Records, execute};
use super::event::Kept;
use super::head::Codec;
use super::pack::{
    HELD, Made, PATIENCE, SEGMENT_EVENTS, SEGMENT_WEIGHT, insert_blocks, insert_keys, made_of, rows_of_blocks,
};
use super::segment::Schema;
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// How many segments one merge joins.
const FAN_IN: usize = 4;
/// A segment under both holds few enough events to merge: four of them fit
/// a segment.
const SMALL_EVENTS: usize = SEGMENT_EVENTS / FAN_IN;
const SMALL_WEIGHT: usize = SEGMENT_WEIGHT / FAN_IN;

/// A segment that holds its own events, few enough to merge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Small {
    id: i64,
    first: i64,
    last: i64,
    held: usize,
    weight: usize,
}

/// A member's row and its blocks' bodies, in their order.
type Member = (Vec<u8>, Vec<Vec<u8>>);

const SMALL: &str = "select id, first_at, last_at, held, weight from segments
    where stream = ?1 and holder is null and held < ?2 and weight < ?3 order by id";
const MEMBER_ROW: &str = "select body from segments where id = ?1 and holder is null";
const MEMBER_BLOCKS: &str = "select body from blocks where segment = ?1 order by id";
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
        let mut merged = 0;
        while let Some(run) = pick(self.small(stream)?) {
            self.merge_run(stream, &run)?;
            merged += run.len() as u64 - 1;
        }
        Ok(merged)
    }

    /// A stream's segments that hold their own events, under a quarter of
    /// what a segment holds.
    fn small(&self, stream: i64) -> Result<Vec<Small>> {
        let failed = |error| sql_error("find the segments to merge", error);
        self.file.read(|connection| {
            let mut select = connection.prepare_cached(SMALL).map_err(failed)?;
            let found = select.query_map(params![stream, SMALL_EVENTS as i64, SMALL_WEIGHT as i64], |row| {
                let (held, weight): (i64, i64) = (row.get(3)?, row.get(4)?);
                let (held, weight) = (held.max(0) as usize, weight.max(0) as usize);
                Ok(Small { id: row.get(0)?, first: row.get(1)?, last: row.get(2)?, held, weight })
            });
            found.and_then(Iterator::collect).map_err(failed)
        })
    }

    /// Merges a run of segments in the order of their times: read them in
    /// one snapshot → parse their events one after another → make one
    /// segment of them → write it in place of them, in one commit.
    fn merge_run(&self, stream: i64, run: &[Small]) -> Result<()> {
        let weight = run.iter().map(|member| member.weight as u64).sum::<u64>();
        let _held = self.memory.reserve(weight * HELD, "records: merging segments", PATIENCE)?;
        let members = self.members(run)?;
        let mut codec = Codec::default();
        let mut events: Vec<Kept> = Vec::new();
        for (member, (row, blocks)) in run.iter().zip(&members) {
            let within = |error: Error| error.within(format!("segment {}", member.id));
            let schema = Schema::parse(row, &mut codec).map_err(within)?;
            for body in blocks {
                events.extend(block::parse(&schema, body, &mut codec).map_err(within)?);
            }
        }
        let made = made_of(&events, &mut codec)?;
        self.publish_merge(stream, run, &made)
    }

    fn members(&self, run: &[Small]) -> Result<Vec<Member>> {
        let failed = |error| sql_error("read the segments to merge", error);
        self.file.read(|connection| {
            let mut row = connection.prepare_cached(MEMBER_ROW).map_err(failed)?;
            let mut blocks = connection.prepare_cached(MEMBER_BLOCKS).map_err(failed)?;
            let mut members = Vec::with_capacity(run.len());
            for member in run {
                let body: Vec<u8> = row.query_row([member.id], |row| row.get(0)).map_err(failed)?;
                let bodies = blocks.query_map([member.id], |row| row.get(0));
                members.push((body, bodies.and_then(Iterator::collect).map_err(failed)?));
            }
            Ok(members)
        })
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
fn in_time_order(mut segments: Vec<Small>) -> Vec<Small> {
    segments.sort_by_key(|segment| (segment.first, segment.id));
    let mut run: Vec<Small> = Vec::with_capacity(FAN_IN);
    for segment in segments {
        if run.last().is_some_and(|last| segment.first < last.last) {
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
