//! Deleting: the events a read finds taken out of their streams, with what
//! the store kept only for them, as a person asks for their data to go.
//!
//! ```text
//! maintenance's lock ─► the heads packed ─► the segments that may hold a match
//!   ─► each in a commit of its own: its slots read, the matches made holes, its row and blocks written again
//!   ─► the WAL emptied into the file, what every commit freed written over with zeros
//! ```
//!
//! A match leaves a hole in its slot, so that a segment's count, its places
//! and a poll's cursor still name every other event where it lay (see
//! `block`), and the events left keep their arrivals, a v0.1.0 block's
//! written down. The segment's row is made again from the events left, its
//! names, contexts and shapes, and so are its keys with their counts and its
//! blocks' filters; its span and its id stay, so that keep removes it when it
//! would have, and the ids of the events deleted are held till then.
//!
//! Packing, merges and expiry wait for the whole delete; appends and reads go
//! on, and a read finds each segment before or after its commit.

use std::sync::PoisonError;

use rusqlite::{OptionalExtension, params};

use super::block;
use super::engine::{Records, Stream, execute};
use super::head::Codec;
use super::merge::DROP_MEMBER;
use super::pack::{HELD, Made, PATIENCE, insert_blocks, insert_keys, made_of_slots, rows_of_blocks};
use super::read::{Query, range};
use super::repair::Part;
use super::segment::Schema;
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// What a delete reads of a segment that holds its events: its row, its
/// blocks in their order, and where its own slots and each place's lie.
struct Holder {
    id: i64,
    stream: i64,
    row: Vec<u8>,
    weight: i64,
    blocks: Vec<(i64, Vec<u8>)>,
    /// Each row that names a range of its slots, its own among them: its
    /// id, where the range begins and how many slots it holds.
    ranges: Vec<(i64, i64, i64)>,
}

const HOLDER: &str = "select stream, body, weight from segments where id = ?1 and holder is null";
const BLOCKS: &str = "select id, body from blocks where segment = ?1 order by id";
const RANGES: &str = "select id, start, count from segments where id = ?1 or holder = ?1";
const HEAD_ROWS: &str = "select min(id), max(id) from heads where stream = ?1 and late = ?2";
const REWRITE: &str = "update segments set weight = ?2, body = ?3 where id = ?1 and holder is null";
const ERASE: &str = "update segments set erased = erased + ?2 where id = ?1";

impl Records {
    /// Deletes what `query` finds in `stream`, or in every stream it names,
    /// and says how many events it deleted.
    pub(super) fn delete(&self, stream: Option<&Stream>, query: &Query) -> Result<u64> {
        query.deletes()?;
        let Some(asked) = self.asked(stream, query)? else { return Ok(0) };
        // what is past keep and not yet removed is deleted too: the range is the query's own
        let span = range(query, self.now())?;
        #[cfg(feature = "sql")]
        self.move_outboxes();
        let _one = self.maintaining.lock().unwrap_or_else(PoisonError::into_inner);
        self.file.zeroing(|| {
            asked.streams.keys().try_for_each(|stream| self.pack_waiting(*stream))?;
            let mut deleted = 0;
            for holder in self.holders_matching(&asked, query, span)? {
                crate::cancel::check(|| "a delete".to_owned())?;
                deleted += self.delete_in(holder, query, span)?;
            }
            Ok(deleted)
        })
    }

    /// Packs the rows a stream's heads hold now, on time and late, so that
    /// every event appended before the delete lies in a segment. A row that
    /// no longer reads holds events no delete reaches: it fails the delete,
    /// listed for a drop.
    fn pack_waiting(&self, stream: i64) -> Result<()> {
        for late in [false, true] {
            let Some((_, last)) = self.head_rows(stream, late)? else { continue };
            while let Some((oldest, _)) = self.head_rows(stream, late)?
                && oldest <= last
            {
                if self.pack_one(stream, late)? == 0 {
                    let why = "a head row that no longer reads, whose events a delete cannot reach: drop it first";
                    return Err(Error::corrupt(why).within(format!("events {}", self.name_of(stream))));
                }
            }
        }
        Ok(())
    }

    /// The oldest and the newest row of a head, if it has one.
    fn head_rows(&self, stream: i64, late: bool) -> Result<Option<(i64, i64)>> {
        let failed = |error| sql_error("read what waits in a head", error);
        self.file.read(|connection| {
            let ends = connection.prepare_cached(HEAD_ROWS).and_then(|mut select| {
                select.query_row(params![stream, late], |row| Ok((row.get::<_, Option<i64>>(0)?, row.get(1)?)))
            });
            let (oldest, newest): (Option<i64>, Option<i64>) = ends.map_err(failed)?;
            Ok(oldest.zip(newest))
        })
    }

    /// Takes the events of the segment `id` that `query` finds in `first` to
    /// `last` out of it, in one commit: its slots read → the matches made
    /// holes → its row, keys and blocks written again. Says how many.
    fn delete_in(&self, id: i64, query: &Query, (first, last): (i64, i64)) -> Result<u64> {
        let Some(holder) = self.holder(id)? else { return Ok(0) };
        let _held = self.memory.reserve(holder.weight.unsigned_abs() * HELD, "records: deleting events", PATIENCE)?;
        let name = self.name_of(holder.stream);
        let within = |error: Error| self.met((Part::Segment, id), &name, error);
        let mut codec = Codec::default();
        let schema = Schema::parse(&holder.row, &mut codec).map_err(within)?;
        let held = holder.ranges.iter().map(|(_, _, count)| count.unsigned_abs() as usize).sum();
        let mut slots = block::segment_slots(&schema, &holder.blocks, held, &mut codec).map_err(within)?;
        let mut holes = Vec::new();
        for (place, slot) in slots.iter_mut().enumerate() {
            if slot.as_ref().is_some_and(|(_, event)| (first..=last).contains(&event.at) && query.keeps(event)) {
                *slot = None;
                holes.push(place as i64);
            }
        }
        if holes.is_empty() {
            return Ok(0);
        }
        let made = made_of_slots(slots, &mut codec)?;
        self.publish_delete(&holder, &holes, &made)?;
        Ok(holes.len() as u64)
    }

    /// A segment that holds its events, read in one snapshot; none once it
    /// holds them no longer.
    fn holder(&self, id: i64) -> Result<Option<Holder>> {
        let failed = |error| sql_error("read a segment to delete from", error);
        self.file.read(|connection| {
            let read = connection.prepare_cached(HOLDER).and_then(|mut select| {
                select.query_row([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()
            });
            let Some((stream, row, weight)) = read.map_err(failed)? else { return Ok(None) };
            let mut blocks = connection.prepare_cached(BLOCKS).map_err(failed)?;
            let blocks = blocks.query_map([id], |row| Ok((row.get(0)?, row.get(1)?)));
            let blocks = blocks.and_then(Iterator::collect).map_err(failed)?;
            let mut ranges = connection.prepare_cached(RANGES).map_err(failed)?;
            let ranges = ranges.query_map([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)));
            let ranges = ranges.and_then(Iterator::collect).map_err(failed)?;
            Ok(Some(Holder { id, stream, row, weight, blocks, ranges }))
        })
    }

    /// Writes a segment again as a delete left it, all or nothing: its
    /// blocks and keys made of the events left, its row of what they share,
    /// and each row naming a range of its slots counting the holes the
    /// delete left in it.
    fn publish_delete(&self, holder: &Holder, holes: &[i64], made: &Made) -> Result<()> {
        let (id, stream) = (holder.id, holder.stream);
        let erased: Vec<(i64, i64)> = holder
            .ranges
            .iter()
            .map(|(row, start, count)| {
                (*row, holes.iter().filter(|hole| (*start..start + count).contains(hole)).count())
            })
            .filter(|(_, holes)| *holes > 0)
            .map(|(row, holes)| (row, holes as i64))
            .collect();
        let bytes = made.blocks.iter().map(|block| block.body.len()).sum();
        let (weight, shared, keys, blocks) =
            (made.weight as i64, made.shared.clone(), made.keys.clone(), rows_of_blocks(made));
        self.file.write(bytes, move |tx| {
            for sql in DROP_MEMBER {
                execute(tx, sql, [id], "remove what a segment held before a delete")?;
            }
            insert_blocks(tx, id, stream, &blocks)?;
            insert_keys(tx, id, &keys)?;
            if execute(tx, REWRITE, params![id, weight, shared], "write a segment a delete left")? != 1 {
                return Err(Error::internal(format!("records: segment {id} changed while events were deleted")));
            }
            for (row, holes) in &erased {
                execute(tx, ERASE, params![row, holes], "count the holes a delete left")?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "delete_tests.rs"]
mod tests;
