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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::PoisonError;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use super::block;
use super::bloom::{bloom_of, id_like};
use super::engine::{Records, execute, nanos};
use super::event::Kept;
use super::head::{self, Codec, ROW_EVENTS, ROW_WEIGHT, Row};
use super::repair::Part;
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
pub(super) use crate::memory::PATIENCE;
/// The segments one pass packs of a stream at most: what is left waits for
/// the next pass, a minute on.
const SWEEPS: usize = 64;

/// Head rows as packing reads them: each one's id and its body.
type HeadRows = Vec<(i64, Vec<u8>, usize)>;

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
    /// What its events hold, by kind: their names, their fields' keys and
    /// their contexts' keys.
    pub(super) keys: BTreeSet<(u8, String)>,
    pub(super) blocks: Vec<Row>,
    pub(super) blooms: Vec<Blooms>,
}

/// What a block's filters say of it: its trace ids, and the values of each
/// of its fields that look like ids.
#[derive(Clone, Debug, Default)]
pub(super) struct Blooms {
    pub(super) traces: Option<Vec<u8>>,
    pub(super) fields: Vec<(String, Vec<u8>)>,
}

/// A block as its rows are written.
pub(super) struct BlockRow {
    first: i64,
    last: i64,
    levels: i64,
    count: i64,
    body: Vec<u8>,
    blooms: Blooms,
}

// the kinds of a segment's keys
pub(super) const NAME_KEY: u8 = 0;
pub(super) const FIELD_KEY: u8 = 1;
pub(super) const CONTEXT_KEY: u8 = 2;

const HEADS: &str = "select stream, late, count, weight, since from head_state";
const HEAD_ROWS: &str = "select id, count, weight, body from heads where stream = ?1 and late = ?2 order by id";
const INSERT_SEGMENT: &str = "insert into segments (stream, first_at, last_at, count, start, held, weight, body)
    values (?1, ?2, ?3, ?4, 0, ?4, ?5, ?6) returning id";
const INSERT_BLOCK: &str = "insert into blocks (segment, stream, first_at, last_at, levels, count, size, body)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) returning id";
const INSERT_TRACES: &str = "insert into block_traces (block, bloom) values (?1, ?2)";
const INSERT_FILTER: &str = "insert into block_filters (block, key, bloom) values (?1, ?2, ?3)";
const INSERT_KEY: &str = "insert into segment_keys (segment, kind, key) values (?1, ?2, ?3)";
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
        // a head whose rows left to pack no longer read waits for their drop
        let mut stuck = BTreeSet::new();
        for _ in 0..SWEEPS {
            let heads = self.heads()?.into_iter();
            let ready: Vec<Head> =
                heads.filter(|head| ready(head, now) && !stuck.contains(&(head.stream, head.late))).collect();
            if ready.is_empty() {
                break;
            }
            for head in ready {
                let events = self.pack_one(head.stream, head.late)?;
                if events == 0 {
                    stuck.insert((head.stream, head.late));
                    continue;
                }
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
    /// rows, in one commit. A row that no longer reads is said once and left
    /// for a drop. Says how many events it packed.
    fn pack_one(&self, stream: i64, late: bool) -> Result<usize> {
        let (rows, weight) = self.oldest_rows(stream, late)?;
        let _held = self.memory.reserve(weight as u64 * HELD, "records: packing a segment", PATIENCE)?;
        let mut codec = Codec::default();
        let (mut events, mut packed, mut packed_weight) = (Vec::new(), Vec::new(), 0);
        for (id, body, heavy) in rows {
            match head::parse(&body, &mut codec) {
                Ok(parsed) => {
                    events.extend(parsed);
                    packed.push(id);
                    packed_weight += heavy;
                }
                Err(error) => drop(self.met((Part::HeadRow, id), &self.name_of(stream), error)),
            }
        }
        if packed.is_empty() {
            return Ok(0);
        }
        // stable: events of one time stay in the order they were written
        events.sort_by_key(|event| event.at);
        let made = made_of(&events, &mut codec)?;
        let emptied = self.publish((stream, late), (packed, packed_weight), &made)?;
        if emptied && !late {
            self.waiting.lock().unwrap_or_else(PoisonError::into_inner).remove(&stream);
        }
        Ok(made.events)
    }

    /// A head's oldest rows that read, a segment's worth and one row at
    /// least, with what each weighs and they all do.
    fn oldest_rows(&self, stream: i64, late: bool) -> Result<(HeadRows, usize)> {
        let failed = |error| sql_error("read the head rows to pack", error);
        self.file.read(|connection| {
            let mut select = connection.prepare_cached(HEAD_ROWS).map_err(failed)?;
            let mut rows = select.query(params![stream, late]).map_err(failed)?;
            let (mut taken, mut events, mut weight) = (Vec::new(), 0, 0);
            while let Some(row) = rows.next().map_err(failed)? {
                let id: i64 = row.get(0).map_err(failed)?;
                if self.is_damaged(Part::HeadRow, id) {
                    continue;
                }
                let (count, heavy): (i64, i64) = (row.get(1).map_err(failed)?, row.get(2).map_err(failed)?);
                let (count, heavy) = (count.max(0) as usize, heavy.max(0) as usize);
                if !taken.is_empty() && (events + count > SEGMENT_EVENTS || weight + heavy > SEGMENT_WEIGHT) {
                    break;
                }
                (events, weight) = (events + count, weight + heavy);
                taken.push((id, row.get(3).map_err(failed)?, heavy));
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
        let (shared, keys) = (made.shared.clone(), made.keys.clone());
        let blocks = rows_of_blocks(made);
        self.file.write(bytes, move |tx| {
            let inserted = tx.prepare_cached(INSERT_SEGMENT);
            let inserted = inserted.and_then(|mut insert| {
                insert.query_row(params![stream, first, last, events, weight, shared], |row| row.get(0))
            });
            let segment: i64 = inserted.map_err(|error| sql_error("write a segment", error))?;
            insert_keys(tx, segment, &keys)?;
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

/// A segment's blocks as their rows are written.
pub(super) fn rows_of_blocks(made: &Made) -> Vec<BlockRow> {
    let row = |(block, blooms): (&Row, &Blooms)| BlockRow {
        first: block.first,
        last: block.last,
        levels: block.levels,
        count: block.count as i64,
        body: block.body.clone(),
        blooms: blooms.clone(),
    };
    made.blocks.iter().zip(&made.blooms).map(row).collect()
}

/// Writes a segment's blocks in their order, each with its filters.
pub(super) fn insert_blocks(tx: &Connection, segment: i64, stream: i64, blocks: &[BlockRow]) -> Result<()> {
    for block in blocks {
        let size = block.body.len() as i64;
        let row = params![segment, stream, block.first, block.last, block.levels, block.count, size, block.body];
        let inserted = tx.prepare_cached(INSERT_BLOCK).and_then(|mut insert| insert.query_row(row, |row| row.get(0)));
        let id: i64 = inserted.map_err(|error| sql_error("write a block", error))?;
        if let Some(traces) = &block.blooms.traces {
            execute(tx, INSERT_TRACES, params![id, traces], "write a block's traces")?;
        }
        for (key, bloom) in &block.blooms.fields {
            execute(tx, INSERT_FILTER, params![id, key, bloom], "write a block's filter")?;
        }
    }
    Ok(())
}

/// Writes what a segment's events hold.
pub(super) fn insert_keys(tx: &Connection, segment: i64, keys: &BTreeSet<(u8, String)>) -> Result<()> {
    keys.iter().try_for_each(|(kind, key)| {
        execute(tx, INSERT_KEY, params![segment, kind, key], "write what a segment holds").map(drop)
    })
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
        keys: keys_of(&schema, events),
        blocks: Vec::new(),
        blooms: Vec::new(),
    };
    let mut from = 0;
    for events in cut(events) {
        let to = from + events.len();
        let ids = [&ids.names[from..to], &ids.shapes[from..to], &ids.contexts[from..to]];
        made.blocks.push(block::encode(&schema, events, ids, codec)?);
        made.blooms.push(blooms_of(events));
        from = to;
    }
    Ok(made)
}

/// The names of a segment's events, the keys of their fields and the keys
/// of their contexts.
fn keys_of(schema: &Schema, events: &[Kept]) -> BTreeSet<(u8, String)> {
    let names = schema.names.iter().map(|name| (NAME_KEY, name.clone()));
    let contexts =
        schema.contexts.iter().flat_map(|context| context.iter().map(|(key, _)| (CONTEXT_KEY, key.to_owned())));
    let fields = events.iter().flat_map(|event| event.fields.iter().map(|(key, _)| (FIELD_KEY, key.to_owned())));
    names.chain(contexts).chain(fields).collect()
}

/// A block's filters: of its trace ids, and of the values of each field
/// whose values in it look like ids.
fn blooms_of(events: &[Kept]) -> Blooms {
    let traces: Vec<&[u8]> =
        events.iter().filter_map(|event| event.trace_id.as_ref()).map(|trace| &trace.0[..]).collect();
    let mut values: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (key, json) in events.iter().flat_map(|event| event.fields.iter()) {
        values.entry(key).or_default().push(json);
    }
    let fields = values.into_iter().filter(|(_, values)| id_like(values));
    Blooms {
        traces: (!traces.is_empty()).then(|| bloom_of(traces.into_iter())),
        fields: fields
            .map(|(key, values)| (key.to_owned(), bloom_of(values.iter().map(|value| value.as_bytes()))))
            .collect(),
    }
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
