//! Packing: a stream's head rows, once they are a segment's worth or an hour
//! old, become a segment, its events in the order of their times.
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

use std::time::Duration;

use rusqlite::params;

use super::block;
use super::engine::{Records, execute, nanos};
use super::event::Kept;
use super::head::{self, Codec, ROW_EVENTS, ROW_WEIGHT, Row};
use super::segment::Schema;
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// The events a segment holds at most, and what they weigh.
const SEGMENT_EVENTS: usize = 16_384;
const SEGMENT_WEIGHT: usize = 4 << 20;
/// How long a quiet stream's events wait before they are packed however few
/// they are, and so how far behind them a poll may be: longer makes fewer,
/// larger segments.
const WAIT: Duration = Duration::from_hours(1);
/// What packing one segment holds of the store's memory for each byte its
/// events weigh: the rows read, the events parsed and sorted, the blocks made.
const HELD: u64 = 4;
/// How long packing waits for the store's memory.
const PATIENCE: Duration = Duration::from_secs(10);
/// The segments one pass packs of a stream at most: what is left waits for
/// the next pass, a minute on.
const SWEEPS: usize = 64;

/// Head rows as packing reads them: each one's id and its body.
type HeadRows = Vec<(i64, Vec<u8>)>;

/// A stream's head: what waits in its rows.
struct Head {
    stream: i64,
    events: usize,
    weight: usize,
    since: i64,
}

/// What one packing took and made.
struct Packed {
    rows: Vec<i64>,
    first: i64,
    last: i64,
    events: usize,
    weight: usize,
    /// The segment's row.
    shared: Vec<u8>,
    blocks: Vec<Row>,
}

const HEADS: &str = "select stream, sum(count), sum(weight), min(written_at) from heads group by stream";
const HEAD_ROWS: &str = "select id, count, weight, body from heads where stream = ?1 order by id";
const INSERT_SEGMENT: &str = "insert into segments (stream, first_at, last_at, count, weight, body)
    values (?1, ?2, ?3, ?4, ?5, ?6) returning id";
const INSERT_BLOCK: &str = "insert into blocks (segment, stream, first_at, last_at, levels, count, size, body)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";
const DROP_HEAD: &str = "delete from heads where id = ?1";

impl Records {
    /// Packs every stream whose head is ready, a segment at a time, and says
    /// how many segments it made of how many events.
    pub(super) fn pack(&self, now: i64) -> Result<(u64, u64)> {
        let (mut segments, mut events) = (0, 0);
        for _ in 0..SWEEPS {
            let ready: Vec<Head> = self.heads()?.into_iter().filter(|head| ready(head, now)).collect();
            if ready.is_empty() {
                break;
            }
            for head in ready {
                let packed = self.pack_one(head.stream)?;
                (segments, events) = (segments + 1, events + packed.events as u64);
            }
        }
        Ok((segments, events))
    }

    fn heads(&self) -> Result<Vec<Head>> {
        let failed = |error| sql_error("read what waits in the heads", error);
        self.file.read(|connection| {
            let mut select = connection.prepare_cached(HEADS).map_err(failed)?;
            let heads = select.query_map([], |row| {
                let (events, weight): (i64, i64) = (row.get(1)?, row.get(2)?);
                Ok(Head {
                    stream: row.get(0)?,
                    events: events.max(0) as usize,
                    weight: weight.max(0) as usize,
                    since: row.get(3)?,
                })
            });
            heads.and_then(Iterator::collect).map_err(failed)
        })
    }

    /// Makes one segment of a stream's oldest head rows: read a segment's
    /// worth → parse → order by time → cut into blocks → write them and
    /// remove the rows, in one commit.
    fn pack_one(&self, stream: i64) -> Result<Packed> {
        let (rows, weight) = self.oldest_rows(stream)?;
        let _held = self.memory.reserve(weight as u64 * HELD, "records: packing a segment", PATIENCE)?;
        let mut codec = Codec::default();
        let mut events = Vec::new();
        for (id, body) in &rows {
            let parsed = head::parse(body, &mut codec).map_err(|error| error.within(format!("head row {id}")))?;
            events.extend(parsed);
        }
        // stable: events of one time stay in the order they were written
        events.sort_by_key(|event| event.at);
        let (Some(first), Some(last)) = (events.first(), events.last()) else {
            return Err(Error::internal("records: a head with no event to pack"));
        };
        let mut packed = Packed {
            rows: rows.iter().map(|(id, _)| *id).collect(),
            first: first.at,
            last: last.at,
            events: events.len(),
            weight,
            shared: Vec::new(),
            blocks: Vec::new(),
        };
        let (schema, ids) = Schema::of(&events);
        packed.shared = schema.encode(&mut codec)?;
        let mut from = 0;
        for events in cut(&events) {
            let to = from + events.len();
            let ids = [&ids.names[from..to], &ids.shapes[from..to], &ids.contexts[from..to]];
            packed.blocks.push(block::encode(&schema, events, ids, &mut codec)?);
            from = to;
        }
        self.publish(stream, &packed)?;
        Ok(packed)
    }

    /// A stream's oldest head rows, a segment's worth and one row at least,
    /// with what they weigh.
    fn oldest_rows(&self, stream: i64) -> Result<(HeadRows, usize)> {
        let failed = |error| sql_error("read the head rows to pack", error);
        self.file.read(|connection| {
            let mut select = connection.prepare_cached(HEAD_ROWS).map_err(failed)?;
            let mut rows = select.query([stream]).map_err(failed)?;
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
    /// of, all or nothing: a row that left meanwhile fails the whole.
    fn publish(&self, stream: i64, packed: &Packed) -> Result<()> {
        let bytes = packed.blocks.iter().map(|block| block.body.len()).sum();
        let (first, last, events, weight) = (packed.first, packed.last, packed.events as i64, packed.weight as i64);
        let (rows, shared) = (packed.rows.clone(), packed.shared.clone());
        let blocks: Vec<(i64, i64, i64, i64, Vec<u8>)> = packed
            .blocks
            .iter()
            .map(|block| (block.first, block.last, block.levels, block.count as i64, block.body.clone()))
            .collect();
        self.file.write(bytes, move |tx| {
            let made = tx.prepare_cached(INSERT_SEGMENT);
            let made = made.and_then(|mut insert| {
                insert.query_row(params![stream, first, last, events, weight, shared], |row| row.get(0))
            });
            let segment: i64 = made.map_err(|error| sql_error("write a segment", error))?;
            for (first, last, levels, count, body) in &blocks {
                let block = params![segment, stream, first, last, levels, count, body.len() as i64, body];
                execute(tx, INSERT_BLOCK, block, "write a block")?;
            }
            for row in &rows {
                if execute(tx, DROP_HEAD, [row], "remove a packed head row")? != 1 {
                    return Err(Error::internal(format!("records: head row {row} left while it was packed")));
                }
            }
            Ok(())
        })
    }
}

/// Whether a head is packed now: it holds a segment's worth, or its oldest
/// row has waited its hour.
fn ready(head: &Head, now: i64) -> bool {
    head.events >= SEGMENT_EVENTS || head.weight >= SEGMENT_WEIGHT || now.saturating_sub(head.since) >= nanos(WAIT)
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
