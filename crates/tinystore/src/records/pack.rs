//! Packing: a head's rows, once they are a segment's worth or an hour old,
//! become a segment, its events in the order of their times. A stream has
//! two heads, its events on time and its late ones, each packed by itself.
//!
//! ```text
//! arrival  .300 buy   .100 menu   .200 save        three head rows
//! packed   .100 menu  .200 save   .300 buy         one segment, in blocks of 1024
//! ```
//!
//! The segment and its blocks are written and the head rows removed in one
//! commit, so a read finds each event once, before, during and after.
//!
//! A segment's row holds what its blocks share, and a block its events by
//! columns: see `segment` and `block`.

use std::collections::BTreeSet;
use std::sync::PoisonError;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use super::block;
use super::engine::{Records, execute, nanos};
use super::event::Kept;
use super::head::{self, Codec, ROW_EVENTS, ROW_WEIGHT, Row};
use super::segment::Schema;
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// The events a segment holds at most, and what they weigh.
pub(super) const SEGMENT_EVENTS: usize = 16_384;
pub(super) const SEGMENT_WEIGHT: usize = 4 << 20;
/// How long a quiet stream's events wait before they are packed however few
/// they are, and so how far behind them a poll may be: longer makes fewer,
/// larger segments.
const WAIT: Duration = Duration::from_hours(1);
/// What packing one segment holds of the store's memory for each byte its
/// events weigh: the rows read, the events parsed and sorted, the blocks made.
pub(super) const HELD: u64 = 4;
/// How long packing waits for the store's memory.
pub(super) const PATIENCE: Duration = Duration::from_secs(10);
/// The segments one pass packs of a stream at most: what is left waits for
/// the next pass, a minute on.
const SWEEPS: usize = 64;

/// Head rows as packing reads them: each one's id and its body.
type HeadRows = Vec<(i64, Vec<u8>)>;

/// A head, a stream's events on time or its late ones: what waits in it.
struct Head {
    stream: i64,
    late: bool,
    events: usize,
    weight: usize,
    since: i64,
}

/// What one pass of packing made, and of which streams.
#[derive(Default)]
pub(super) struct Packing {
    pub(super) segments: u64,
    pub(super) events: u64,
    pub(super) streams: BTreeSet<i64>,
}

/// A segment made of events in the order of their times.
pub(super) struct Made {
    pub(super) first: i64,
    pub(super) last: i64,
    pub(super) events: usize,
    pub(super) weight: usize,
    /// The segment's row.
    pub(super) shared: Vec<u8>,
    pub(super) blocks: Vec<Row>,
}

const HEADS: &str = "select stream, late, count, weight, since from head_state";
const HEAD_ROWS: &str = "select id, count, weight, body from heads where stream = ?1 and late = ?2 order by id";
const INSERT_SEGMENT: &str = "insert into segments (stream, first_at, last_at, count, start, held, weight, body)
    values (?1, ?2, ?3, ?4, 0, ?4, ?5, ?6) returning id";
pub(super) const INSERT_BLOCK: &str = "insert into blocks
    (segment, stream, first_at, last_at, levels, count, size, body) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";
const DROP_HEAD: &str = "delete from heads where id = ?1";
const SUBTRACT_FROM_HEAD: &str =
    "update head_state set count = count - ?3, weight = weight - ?4 where stream = ?1 and late = ?2";
const OLDEST_HEAD_ROW: &str = "select written_at from heads where stream = ?1 and late = ?2 order by id limit 1";
const SET_HEAD_SINCE: &str = "update head_state set since = ?3 where stream = ?1 and late = ?2";
const DROP_HEAD_STATE: &str = "delete from head_state where stream = ?1 and late = ?2";

impl Records {
    /// Packs every head that is ready, a segment at a time.
    pub(super) fn pack(&self, now: i64) -> Result<Packing> {
        let mut packing = Packing::default();
        for _ in 0..SWEEPS {
            let ready: Vec<Head> = self.heads()?.into_iter().filter(|head| ready(head, now)).collect();
            if ready.is_empty() {
                break;
            }
            for head in ready {
                let events = self.pack_one(head.stream, head.late)?;
                (packing.segments, packing.events) = (packing.segments + 1, packing.events + events as u64);
                packing.streams.insert(head.stream);
            }
        }
        Ok(packing)
    }

    fn heads(&self) -> Result<Vec<Head>> {
        let failed = |error| sql_error("read what waits in the heads", error);
        self.file.read(|connection| {
            let mut select = connection.prepare_cached(HEADS).map_err(failed)?;
            let heads = select.query_map([], |row| {
                let (events, weight): (i64, i64) = (row.get(2)?, row.get(3)?);
                Ok(Head {
                    stream: row.get(0)?,
                    late: row.get(1)?,
                    events: events.max(0) as usize,
                    weight: weight.max(0) as usize,
                    since: row.get(4)?,
                })
            });
            heads.and_then(Iterator::collect).map_err(failed)
        })
    }

    /// Makes one segment of a head's oldest rows: read a segment's worth →
    /// parse → order by time → make the segment → write it and remove the
    /// rows, in one commit. Says how many events it packed.
    fn pack_one(&self, stream: i64, late: bool) -> Result<usize> {
        let (rows, weight) = self.oldest_rows(stream, late)?;
        let _held = self.memory.reserve(weight as u64 * HELD, "records: packing a segment", PATIENCE)?;
        let mut codec = Codec::default();
        let mut events = Vec::new();
        for (id, body) in &rows {
            let parsed = head::parse(body, &mut codec).map_err(|error| error.within(format!("head row {id}")))?;
            events.extend(parsed);
        }
        // stable: events of one time stay in the order they were written
        events.sort_by_key(|event| event.at);
        let made = made_of(&events, &mut codec)?;
        let rows: Vec<i64> = rows.iter().map(|(id, _)| *id).collect();
        let emptied = self.publish((stream, late), (rows, weight), &made)?;
        if emptied && !late {
            self.waiting.lock().unwrap_or_else(PoisonError::into_inner).remove(&stream);
        }
        Ok(made.events)
    }

    /// A head's oldest rows, a segment's worth and one row at least, with
    /// what they weigh.
    fn oldest_rows(&self, stream: i64, late: bool) -> Result<(HeadRows, usize)> {
        let failed = |error| sql_error("read the head rows to pack", error);
        self.file.read(|connection| {
            let mut select = connection.prepare_cached(HEAD_ROWS).map_err(failed)?;
            let mut rows = select.query(params![stream, late]).map_err(failed)?;
            let (mut taken, mut events, mut weight) = (Vec::new(), 0, 0);
            while let Some(row) = rows.next().map_err(failed)? {
                let (count, heavy): (i64, i64) = (row.get(1).map_err(failed)?, row.get(2).map_err(failed)?);
                let (count, heavy) = (count.max(0) as usize, heavy.max(0) as usize);
                if !taken.is_empty() && (events + count > SEGMENT_EVENTS || weight + heavy > SEGMENT_WEIGHT) {
                    break;
                }
                (events, weight) = (events + count, weight + heavy);
                taken.push((row.get(0).map_err(failed)?, row.get(3).map_err(failed)?));
            }
            Ok((taken, weight))
        })
    }

    /// Writes a segment with its blocks and removes the head rows it was made
    /// of, `weight` what they weigh, all or nothing: a row that left
    /// meanwhile fails the whole. Says whether the head is empty now.
    fn publish(&self, (stream, late): (i64, bool), (rows, weight): (Vec<i64>, usize), made: &Made) -> Result<bool> {
        let bytes = made.blocks.iter().map(|block| block.body.len()).sum();
        let (first, last, events, weight) = (made.first, made.last, made.events as i64, weight as i64);
        let shared = made.shared.clone();
        let blocks = rows_of_blocks(&made.blocks);
        self.file.write(bytes, move |tx| {
            let inserted = tx.prepare_cached(INSERT_SEGMENT);
            let inserted = inserted.and_then(|mut insert| {
                insert.query_row(params![stream, first, last, events, weight, shared], |row| row.get(0))
            });
            let segment: i64 = inserted.map_err(|error| sql_error("write a segment", error))?;
            insert_blocks(tx, segment, stream, &blocks)?;
            for row in &rows {
                if execute(tx, DROP_HEAD, [row], "remove a packed head row")? != 1 {
                    return Err(Error::internal(format!("records: head row {row} left while it was packed")));
                }
            }
            settle(tx, (stream, late), events, weight)
        })
    }
}

/// A block as its row is written: its times, levels, events and body.
pub(super) type BlockRow = (i64, i64, i64, i64, Vec<u8>);

pub(super) fn rows_of_blocks(blocks: &[Row]) -> Vec<BlockRow> {
    let row = |block: &Row| (block.first, block.last, block.levels, block.count as i64, block.body.clone());
    blocks.iter().map(row).collect()
}

/// Writes a segment's blocks, in their order.
pub(super) fn insert_blocks(tx: &Connection, segment: i64, stream: i64, blocks: &[BlockRow]) -> Result<()> {
    for (first, last, levels, count, body) in blocks {
        let block = params![segment, stream, first, last, levels, count, body.len() as i64, body];
        execute(tx, INSERT_BLOCK, block, "write a block")?;
    }
    Ok(())
}

/// Takes what left a head, packed or expired, from what it holds: its oldest
/// row left says since when it waits, and a head with none left is gone.
/// Says whether it is.
pub(super) fn settle(tx: &Connection, (stream, late): (i64, bool), events: i64, weight: i64) -> Result<bool> {
    let what = "count what a head holds";
    execute(tx, SUBTRACT_FROM_HEAD, params![stream, late, events, weight], what)?;
    let oldest = tx.prepare_cached(OLDEST_HEAD_ROW);
    let oldest =
        oldest.and_then(|mut select| select.query_row(params![stream, late], |row| row.get::<_, i64>(0)).optional());
    match oldest.map_err(|error| sql_error(what, error))? {
        Some(since) => execute(tx, SET_HEAD_SINCE, params![stream, late, since], what).map(|_| false),
        None => execute(tx, DROP_HEAD_STATE, params![stream, late], what).map(|_| true),
    }
}

/// Whether a head is packed now: it holds a segment's worth, or its oldest
/// row has waited its hour.
fn ready(head: &Head, now: i64) -> bool {
    head.events >= SEGMENT_EVENTS || head.weight >= SEGMENT_WEIGHT || now.saturating_sub(head.since) >= nanos(WAIT)
}

/// A segment of `events`, which are in the order of their times: what its
/// blocks share, and the blocks.
pub(super) fn made_of(events: &[Kept], codec: &mut Codec) -> Result<Made> {
    let (Some(first), Some(last)) = (events.first(), events.last()) else {
        return Err(Error::internal("records: a segment of no event"));
    };
    let (schema, ids) = Schema::of(events);
    let mut made = Made {
        first: first.at,
        last: last.at,
        events: events.len(),
        weight: events.iter().map(Kept::weight).sum(),
        shared: schema.encode(codec)?,
        blocks: Vec::new(),
    };
    let mut from = 0;
    for events in cut(events) {
        let to = from + events.len();
        let ids = [&ids.names[from..to], &ids.shapes[from..to], &ids.contexts[from..to]];
        made.blocks.push(block::encode(&schema, events, ids, codec)?);
        from = to;
    }
    Ok(made)
}

/// A segment's events cut into its blocks: each a block's events at most, or
/// what they weigh; one event past the weight still makes a block.
pub(super) fn cut(events: &[Kept]) -> Vec<&[Kept]> {
    let (mut blocks, mut from, mut weight) = (Vec::new(), 0, 0);
    for (index, event) in events.iter().enumerate() {
        let added = event.weight();
        if index > from && (index - from == ROW_EVENTS || weight + added > ROW_WEIGHT) {
            blocks.push(&events[from..index]);
            (from, weight) = (index, 0);
        }
        weight += added;
    }
    blocks.push(&events[from..]);
    blocks
}

#[cfg(test)]
#[path = "pack_tests.rs"]
mod tests;
