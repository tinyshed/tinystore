//! Polls: every packed event once, a batch a call, from a cursor the program
//! keeps, in the order segments were packed. A batch is one segment's events,
//! one stream's, in the order of their times; a merged segment's are read
//! where its holder keeps them, so that a cursor that names it still finds
//! each of them once.
//!
//! ```text
//! cursor "17:300"   segment 17, its first 300 events given
//! segment 17, a place in 12 from 400 on   → 12's events 700.., as many as the batch takes
//! ```
//!
//! A segment its stream's `keep` removed before the cursor reached it is
//! counted in `expired`, by the events each held, which the file keeps for
//! thirty days; past them a segment counts one, so that more than none still
//! says the copy has a hole. Head rows `keep` removed before they were packed
//! are counted as a segment of their own.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};

use super::block;
use super::engine::Records;
use super::event::Event;
use super::events::AllEvents;
use super::head::Codec;
use super::pack::PATIENCE;
use super::read::EVENT_HELD;
use super::repair::Part;
use super::segment::Schema;
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// The events of a batch unless a poll says, and at most.
const POLL_EVENTS: usize = 1000;
const MOST_POLL: usize = 10_000;

/// A batch of `poll`: its events, the cursor after them, and how many events
/// their streams' `keep` removed before the cursor reached them.
#[derive(Clone, Debug, PartialEq)]
pub struct PollBatch {
    pub events: Vec<Event>,
    pub cursor: String,
    pub expired: u64,
}

impl AllEvents {
    /// Every packed event once, a batch a call, 1000 at most: from `cursor`,
    /// a batch's, or from the oldest event kept without one. With no event
    /// past the cursor, the batch is empty and its cursor the one given;
    /// with no event at all, the cursor where the next packed ones begin.
    pub fn poll(&self, cursor: Option<&str>) -> Result<PollBatch> {
        self.poll_limit(cursor, POLL_EVENTS)
    }

    /// As `poll`, `limit` events at most, 10,000 at the most.
    pub fn poll_limit(&self, cursor: Option<&str>, limit: usize) -> Result<PollBatch> {
        if !(1..=MOST_POLL).contains(&limit) {
            return Err(Error::invalid(format!("a poll of {limit} events: it takes 1 to {MOST_POLL}")).within("events"));
        }
        let from = cursor.map(parse).transpose().map_err(|error| error.within("events"))?;
        self.records.poll(from, limit).map_err(|error| error.within("events"))
    }
}

/// The segment and the place among its events a cursor names.
fn parse(cursor: &str) -> Result<(i64, i64)> {
    let place =
        cursor.split_once(':').and_then(|(segment, offset)| Some((segment.parse().ok()?, offset.parse().ok()?)));
    place
        .filter(|(segment, offset)| *segment >= 0 && *offset >= 0)
        .ok_or_else(|| Error::invalid(format!("cursor {cursor:?}, which is no poll's")))
}

/// What one read of the file fetched for a batch.
struct Fetched {
    segment: i64,
    /// The segment whose bytes the batch reads: the segment's own id, or the
    /// one a merge put its events in, under which damage is listed.
    holder: i64,
    stream: i64,
    /// Where the batch's events lie among its holder's, and the place among
    /// the segment's own events where the batch begins.
    from: i64,
    to: i64,
    skip: i64,
    count: i64,
    /// The holder's row, and the bodies of its blocks that hold the batch,
    /// each with where its events begin.
    row: Vec<u8>,
    blocks: Vec<(i64, Vec<u8>)>,
    expired: u64,
}

const NEXT_SEGMENT: &str = "select id, stream, holder, start, count from segments where id >= ?1 order by id limit 1";
const SEGMENT_ROW: &str = "select body from segments where id = ?1";
const BLOCKS: &str = "select id, count from blocks where segment = ?1 order by id";
const BLOCK_BODY: &str = "select body from blocks where id = ?1";
const EXPIRED: &str = "select segment, events from expired where segment >= ?1 and segment < ?2";
/// The id the next segment packed takes: ids only grow, and those of every
/// segment removed stay taken.
const NEXT_ID: &str = "select coalesce((select seq from sqlite_sequence where name = 'segments'), 0) + 1";

/// What a poll's read of the file found: a batch, or no segment at the
/// cursor or past it, and the id the next segment packed takes.
enum Found {
    Batch(Box<Fetched>),
    Nothing { next: i64 },
}

impl Records {
    /// A batch: the first segment at the cursor or past it, read in one
    /// moment of the file, its events built after.
    fn poll(&self, from: Option<(i64, i64)>, limit: usize) -> Result<PollBatch> {
        let (segment, offset) = from.unwrap_or((0, 0));
        let fetched = match self.fetch_batch((segment, offset), from.is_some(), limit)? {
            Found::Batch(fetched) => fetched,
            // without a cursor, the next segment's: a poll from it counts only what keep removes after
            Found::Nothing { next } => {
                let cursor = if from.is_some() { format!("{segment}:{offset}") } else { format!("{next}:0") };
                return Ok(PollBatch { events: Vec::new(), cursor, expired: 0 });
            }
        };
        // the bodies fetched and the events built of them hold the store's memory, as a read's do
        let fetched_bytes = fetched.row.len() + fetched.blocks.iter().map(|(_, body)| body.len()).sum::<usize>();
        let events = (fetched.to - fetched.from).max(0) as u64;
        let _held = self.memory.reserve(fetched_bytes as u64 + events * EVENT_HELD, "records: a poll", PATIENCE)?;
        let name = self.name_of(fetched.stream);
        let mut codec = Codec::default();
        let within = |error: Error| self.met((Part::Segment, fetched.holder), &name, error);
        let schema = Schema::parse(&fetched.row, &mut codec).map_err(within)?;
        let mut events = Vec::with_capacity((fetched.to - fetched.from) as usize);
        for (begins, body) in &fetched.blocks {
            let parsed = block::parse(&schema, body, &mut codec).map_err(within)?;
            let (low, high) =
                ((fetched.from - begins).max(0) as usize, (fetched.to - begins).min(parsed.len() as i64) as usize);
            events.extend(parsed.into_iter().take(high).skip(low).map(|event| event.into_event(&name)));
        }
        let read = fetched.skip + events.len() as i64;
        let cursor = if read < fetched.count {
            format!("{}:{read}", fetched.segment)
        } else {
            format!("{}:0", fetched.segment + 1)
        };
        Ok(PollBatch { events, cursor, expired: fetched.expired })
    }

    fn fetch_batch(&self, (segment, offset): (i64, i64), counted: bool, limit: usize) -> Result<Found> {
        let failed = |error| sql_error("read a poll's batch", error);
        self.file.read(|connection| {
            let next = connection.prepare_cached(NEXT_SEGMENT).and_then(|mut select| {
                select
                    .query_row([segment], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get::<_, Option<i64>>(2)?, row.get(3)?, row.get(4)?))
                    })
                    .optional()
            });
            let Some((id, stream, holder, start, count)): Option<(i64, i64, Option<i64>, i64, i64)> =
                next.map_err(failed)?
            else {
                let next =
                    connection.prepare_cached(NEXT_ID).and_then(|mut select| select.query_row([], |row| row.get(0)));
                return Ok(Found::Nothing { next: next.map_err(failed)? });
            };
            let expired = if counted { expired_between(connection, (segment, offset), id)? } else { 0 };
            let skip = if id == segment { offset.min(count) } else { 0 };
            let holder = holder.unwrap_or(id);
            let (from, to) = (start + skip, start + count.min(skip + limit as i64));
            let mut blocks = Vec::new();
            let mut select = connection.prepare_cached(BLOCKS).map_err(failed)?;
            let counts = select.query_map([holder], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)));
            let mut begins = 0;
            for block in counts.map_err(failed)? {
                let (block, events) = block.map_err(failed)?;
                if begins < to && begins + events > from {
                    let body = connection
                        .prepare_cached(BLOCK_BODY)
                        .and_then(|mut select| select.query_row([block], |row| row.get(0)));
                    blocks.push((begins, body.map_err(failed)?));
                }
                begins += events;
            }
            let row = connection
                .prepare_cached(SEGMENT_ROW)
                .and_then(|mut select| select.query_row([holder], |row| row.get(0)));
            let row = row.map_err(failed)?;
            Ok(Found::Batch(Box::new(Fetched {
                segment: id,
                holder,
                stream,
                from,
                to,
                skip,
                count,
                row,
                blocks,
                expired,
            })))
        })
    }
}

/// The events of the segments from the cursor's to `found`, the first it
/// found, which their streams' `keep` removed before it reached them: what
/// the file kept of each, one a segment it no longer keeps, and of the
/// cursor's own those past its place.
fn expired_between(connection: &rusqlite::Connection, (segment, offset): (i64, i64), found: i64) -> Result<u64> {
    if found <= segment {
        return Ok(0);
    }
    let failed = |error| sql_error("count what a poll never read", error);
    let mut select = connection.prepare_cached(EXPIRED).map_err(failed)?;
    let rows = select.query_map(params![segment, found], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)));
    let held: HashMap<i64, i64> = rows.and_then(Iterator::collect).map_err(failed)?;
    // no segment is 0: a cursor given before any was packed passed none of it
    let own = match segment {
        0 => 0,
        _ => held.get(&segment).map_or(1, |events| (events - offset).max(1)),
    };
    let others: i64 = held.iter().filter(|(id, _)| **id != segment).map(|(_, events)| *events).sum();
    let unknown = (found - segment - 1) - held.keys().filter(|id| **id != segment).count() as i64;
    Ok((own + others + unknown).max(0) as u64)
}

#[cfg(test)]
#[path = "poll_tests.rs"]
mod tests;
