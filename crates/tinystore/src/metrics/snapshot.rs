//! One snapshot of the file, copied: the series a query finds and the bytes
//! their ranges touch.
//!
//! ```text
//! find the series ─► copy their directories, clocks, payloads and heads ─► reserve the store's memory ─► decode
//! └────────────────── one read of the file ────────────────┘   └────────────── after it ──────────────┘
//! ```
//!
//! A read transaction covers copying bytes, never decoding samples, so that a
//! heavy query is not itself a long reader: the write-ahead log's size is the
//! age of the oldest one. Every row's bytes are paid for before they are
//! copied, and what a call does not need is not copied: a block an aggregate
//! answers from its summary keeps its body in the file, and `latest` takes
//! one chunk of a head or one block's directory.

use rusqlite::{Connection, OptionalExtension, params};

use super::aggregate::{Op, Shape};
use super::budget::Budget;
use super::clock::{self, Times};
use super::codec::Point;
use super::directory::{Block, Group, Row};
use super::engine::Engine;
use super::head::{self, Chunk, Extent};
use super::matching::{self, Found};
use super::names::{KEEP, millis};
use super::query::Asked;
use super::timer;
use super::values;
use crate::compress::frame::Zstd;
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// The decoded clocks a snapshot keeps, since series written together share
/// theirs.
const KEPT_CLOCKS: usize = 8;

/// What a call takes of each series.
#[derive(Clone, Copy, Debug)]
pub(super) enum Want<'a> {
    /// Every sample of the range: each block and each chunk of the head it
    /// touches.
    Samples,
    /// An aggregate's buckets: a block inside one by its summary, and for an
    /// `op` that counts steps the newest sample before the range.
    Buckets(&'a Shape),
    /// The newest sample of the range alone.
    Newest,
}

/// The times one series is read over.
#[derive(Clone, Copy, Debug)]
pub(super) struct Window {
    /// Where its samples count from: the query's start, or the oldest time
    /// its name keeps, whichever is later.
    pub(super) from: i64,
    pub(super) to: i64,
    /// The oldest time its name keeps: a sample before it is never read,
    /// whether or not it has left the file.
    pub(super) cutoff: i64,
}

/// A series as one snapshot of the file held it.
#[derive(Debug)]
pub(super) struct Taken {
    pub(super) found: Found,
    pub(super) window: Window,
    /// Retention has taken samples of it, or the file still holds some its
    /// name keeps no longer: the step into its oldest sample began at one.
    pub(super) trimmed: bool,
    /// Its live blocks the call needs, in the order of their times.
    pub(super) blocks: Vec<Part>,
    pub(super) head: Option<TakenHead>,
}

/// A block of a snapshot.
#[derive(Debug)]
pub(super) struct Part {
    pub(super) block: Block,
    /// Answered from its directory: its body stayed in the file.
    pub(super) whole: bool,
}

/// A head of a snapshot: its bytes, and where the call decodes it from.
#[derive(Debug)]
pub(super) struct TakenHead {
    extent: Extent,
    bytes: Vec<u8>,
    /// The chunks that end at or after it are decoded.
    from: i64,
}

/// A series' state, and its head's bytes when the window touches it and they
/// fit what the call may still copy.
const STATE: &str = "select s.head_count, s.head_start, s.head_end,
    s.trimmed = 1 or (s.due is not null and s.due <= ?5), length(h.body),
    case when s.head_end >= ?2 and s.head_start < ?3 and length(h.body) <= ?4 then h.body end
    from series_state s left join heads h on h.series_id = s.series_id where s.series_id = ?1";
/// The groups that may hold samples of `[?2, ?3)`: from the last that starts
/// at or before `?2`, so that the index walks no older one.
const GROUPS: &str = "select start_ts, end_ts, clock, length(directory),
    case when length(directory) <= ?5 then directory end
    from groups where series_id = ?1
    and start_ts >= coalesce((select start_ts from groups where series_id = ?1 and start_ts <= ?2
        order by start_ts desc limit 1), ?2)
    and start_ts < ?3 and end_ts >= ?2 order by start_ts";
/// Those, and before them the last group that starts before `?2`, when the
/// name still keeps its end, `?4`: it holds the newest sample before the
/// range, which a first step begins at.
const GROUPS_AND_BEFORE: &str = "select start_ts, end_ts, clock, length(directory),
    case when length(directory) <= ?5 then directory end
    from groups where series_id = ?1
    and start_ts >= coalesce((select start_ts from groups where series_id = ?1 and start_ts < ?2
        order by start_ts desc limit 1), ?2)
    and start_ts < ?3 and end_ts >= ?4 order by start_ts";
/// The last group that starts before `?3`, when it ends in the range. The
/// group is found by its id first, so that one that ends before the range
/// costs no directory.
const NEWEST_GROUP: &str = "select start_ts, end_ts, clock, length(directory),
    case when length(directory) <= ?5 then directory end
    from groups where id = (select id from groups where series_id = ?1 and start_ts < ?3
        order by start_ts desc limit 1)
    and end_ts >= ?2";
const PAYLOAD: &str = "select body from payloads where id = ?1";

impl Engine {
    /// Copies what a call needs out of one snapshot of the file: the series
    /// its query finds, and of each the bytes the call takes.
    pub(super) fn take(&self, asked: &Asked, want: Want<'_>, budget: &mut Budget) -> Result<Vec<Taken>> {
        let mut zstd = self.codecs.take();
        let taken = self.file.read(|connection| {
            let read_as = timer::read_as(connection, asked, want)?;
            let asked = read_as.as_ref().unwrap_or(asked);
            let found = matching::find(connection, asked, budget)?;
            let mut reading = Reading { connection, zstd: &mut zstd, budget, clocks: Vec::new() };
            found.into_iter().map(|series| reading.series(self, series, asked, want)).collect()
        });
        self.codecs.give_back(zstd);
        taken
    }
}

/// One snapshot being copied: its connection, the budget it pays from, and
/// the clocks it decoded.
struct Reading<'r, 'c> {
    connection: &'c Connection,
    zstd: &'r mut Zstd,
    budget: &'r mut Budget,
    clocks: Vec<(i64, Vec<Times>)>,
}

/// A series' row of state, as a snapshot reads it.
struct State {
    extent: Extent,
    trimmed: bool,
    head_len: usize,
    head: Option<Vec<u8>>,
}

impl Reading<'_, '_> {
    fn series(&mut self, engine: &Engine, found: Found, asked: &Asked, want: Want<'_>) -> Result<Taken> {
        let keep = engine.names.get(found.kept.name()).map_or(KEEP, |name| name.keeps());
        let cutoff = asked.now.saturating_sub(millis(keep));
        let window = Window { from: asked.from.max(cutoff), to: asked.to, cutoff };
        let state = self.state(&found, window, asked.now)?;
        let (extent, trimmed) = (state.extent, state.trimmed);
        let mut taken = Taken { found, window, trimmed, blocks: Vec::new(), head: None };
        // every block is older than the head, so a head that ends before the
        // window leaves it nothing
        if extent.count > 0 && extent.end < window.from {
            return Ok(taken);
        }
        taken.head = self.head(&taken.found, state, window, want)?;
        let id = taken.found.id;
        let blocks = match want {
            Want::Newest if taken.head.is_some() => Ok(Vec::new()),
            Want::Newest => self.newest_block(id, window),
            Want::Samples => self.blocks(id, window, None, false),
            Want::Buckets(shape) => {
                // a head that starts before the range holds the sample a first step begins at
                let in_head = extent.count > 0 && extent.start < window.from;
                let op = shape.series_op(&taken.found);
                let before = Shape::steps(op) && window.from > cutoff && !in_head;
                self.blocks(id, window, Some((shape, op)), before)
            }
        };
        taken.blocks = blocks.map_err(|error| error.within(&taken.found.kept))?;
        Ok(taken)
    }

    fn state(&mut self, found: &Found, window: Window, now: i64) -> Result<State> {
        let room = self.budget.bytes_left() as i64;
        let row = self.connection.prepare_cached(STATE).and_then(|mut select| {
            let asked = params![found.id, window.from, window.to, room, now];
            let read = select.query_row(asked, |row| {
                let extent = (row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?, row.get::<_, Option<i64>>(2)?);
                Ok((extent, row.get::<_, bool>(3)?, row.get::<_, Option<i64>>(4)?, row.get::<_, Option<Vec<u8>>>(5)?))
            });
            read.optional()
        });
        let row = row.map_err(|error| sql_error("read a series' state", error))?;
        let Some(((count, start, end), trimmed, length, head)) = row else {
            return Err(Error::corrupt(format!("{}: a series without its state", found.kept)));
        };
        let extent =
            Extent { count: usize::try_from(count).unwrap_or(0), start: start.unwrap_or(0), end: end.unwrap_or(0) };
        let head_len = usize::try_from(length.unwrap_or(0)).unwrap_or(usize::MAX);
        Ok(State { extent, trimmed, head_len, head })
    }

    /// A series' head when the window touches it, its bytes and the samples
    /// of the chunks the call decodes paid for.
    fn head(&mut self, found: &Found, state: State, window: Window, want: Want<'_>) -> Result<Option<TakenHead>> {
        let extent = state.extent;
        if extent.count == 0 || extent.end < window.from || extent.start >= window.to {
            return Ok(None);
        }
        self.budget.take_bytes(state.head_len)?;
        let bytes =
            state.head.ok_or_else(|| Error::corrupt(format!("{}: a head its state says it has", found.kept)))?;
        let chunks = head::parse(found.id, extent, &bytes).map_err(|error| error.within(&found.kept))?;
        let from = match want {
            Want::Buckets(shape) if Shape::steps(shape.series_op(found)) => from_the_chunk_before(&chunks, window),
            Want::Samples | Want::Buckets(_) => window.from,
            Want::Newest => from_the_newest_chunk(&chunks, window),
        };
        self.budget.take_decoded(head::samples_within(&chunks, from, window.to))?;
        Ok(Some(TakenHead { extent, bytes, from }))
    }

    /// The live blocks of a series the window touches, in the order of their
    /// times, and with `before` the newest that ends before it, of which only
    /// the last sample counts. A block an aggregate's `shape` answers from
    /// its summary for the series' `op` is paid for by its directory alone;
    /// any other by its samples, and by its payload's bytes before they are
    /// copied.
    fn blocks(&mut self, id: i64, window: Window, shape: Option<(&Shape, Op)>, before: bool) -> Result<Vec<Part>> {
        let rows = self.group_rows(if before { GROUPS_AND_BEFORE } else { GROUPS }, id, window)?;
        let mut parts = Vec::new();
        let mut carried = None;
        for row in rows {
            let clock = self.clock(row.clock)?;
            let group = Group::decode(self.zstd, &row, clock)?;
            for (slot, block) in group.blocks.into_iter().enumerate() {
                if group.live >> slot & 1 == 0 || block.times.start >= window.to {
                    continue;
                }
                if block.times.end < window.from {
                    if before && block.times.end >= window.cutoff {
                        carried = Some(block);
                    }
                    continue;
                }
                let whole = shape.is_some_and(|(shape, op)| shape.holds(op, &block, window));
                parts.push(self.part(block, whole)?);
            }
        }
        parts.splice(0..0, carried.map(|block| Part { block, whole: true }));
        Ok(parts)
    }

    /// The newest live block of a series that starts before the window's
    /// end, when it ends in the window: answered from its directory when its
    /// last sample is the newest of the range, decoded when the range ends
    /// inside it.
    fn newest_block(&mut self, id: i64, window: Window) -> Result<Vec<Part>> {
        let Some(row) = self.group_rows(NEWEST_GROUP, id, window)?.into_iter().next() else { return Ok(Vec::new()) };
        let clock = self.clock(row.clock)?;
        let group = Group::decode(self.zstd, &row, clock)?;
        let live = group.live;
        let mut blocks = group.blocks.into_iter().enumerate().rev();
        let newest = blocks.find(|(slot, block)| live >> slot & 1 == 1 && block.times.start < window.to);
        match newest {
            Some((_, block)) if block.times.end >= window.from => {
                let whole = block.times.end < window.to;
                Ok(vec![self.part(block, whole)?])
            }
            _ => Ok(Vec::new()),
        }
    }

    fn part(&mut self, mut block: Block, whole: bool) -> Result<Part> {
        if !whole {
            self.budget.take_decoded(block.times.count)?;
            if block.external() {
                self.budget.take_bytes(block.body_len)?;
                // a plan counts a payload's bytes by its directory and leaves them in the file
                if !self.budget.is_planning() {
                    block.body = self.payload(block.payload, block.body_len)?;
                }
            }
        }
        Ok(Part { block, whole })
    }

    fn group_rows(&mut self, sql: &'static str, id: i64, window: Window) -> Result<Vec<Row>> {
        let failed = |error| sql_error("read a series' groups", error);
        let mut select = self.connection.prepare_cached(sql).map_err(failed)?;
        let room = self.budget.bytes_left() as i64;
        let mut rows = select.query(params![id, window.from, window.to, window.cutoff, room]).map_err(failed)?;
        let mut found = Vec::new();
        while let Some(row) = rows.next().map_err(failed)? {
            let length: i64 = row.get(3).map_err(failed)?;
            self.budget.take_bytes(usize::try_from(length).unwrap_or(usize::MAX))?;
            let directory: Option<Vec<u8>> = row.get(4).map_err(failed)?;
            let directory = directory.ok_or_else(|| Error::corrupt("a group's directory past its length"))?;
            let (start, end) = (row.get(0).map_err(failed)?, row.get(1).map_err(failed)?);
            found.push(Row { series: id, start, end, clock: row.get(2).map_err(failed)?, directory });
        }
        Ok(found)
    }

    /// A clock this snapshot decoded, or the file's, paid for.
    fn clock(&mut self, id: i64) -> Result<Vec<Times>> {
        if let Some((_, times)) = self.clocks.iter().find(|(kept, _)| *kept == id) {
            return Ok(times.clone());
        }
        let (length, body) = clock::load(self.connection, id, self.budget.bytes_left())?;
        self.budget.take_bytes(length)?;
        let times = clock::decode(&body.ok_or_else(|| Error::corrupt("a clock past its length"))?)?;
        if self.clocks.len() == KEPT_CLOCKS {
            self.clocks.remove(0);
        }
        self.clocks.push((id, times.clone()));
        Ok(times)
    }

    fn payload(&mut self, id: i64, length: usize) -> Result<Vec<u8>> {
        let body: Option<Vec<u8>> = self
            .connection
            .prepare_cached(PAYLOAD)
            .and_then(|mut select| select.query_row(params![id], |row| row.get(0)).optional())
            .map_err(|error| sql_error("read a block's values", error))?;
        let body = body.filter(|body| body.len() == length);
        body.ok_or_else(|| Error::corrupt("a block's payload is missing or of another length"))
    }
}

/// Where a head is decoded from for a first step: the last chunk that starts
/// before the window, which holds the newest sample before it, when the name
/// still keeps its end.
///
/// ```text
/// chunks    [09:00 … 09:59] [10:00 … 10:59] [11:00 … 11:20]
/// window                                     11:00 ─────────
/// decoded                   [10:00 … 10:59] [11:00 … 11:20]
/// ```
fn from_the_chunk_before(chunks: &[Chunk<'_>], window: Window) -> i64 {
    let before = chunks.iter().rev().find(|chunk| chunk.head.start < window.from);
    let kept = before.filter(|chunk| chunk.head.end >= window.cutoff);
    kept.map_or(window.from, |chunk| chunk.head.end.min(window.from))
}

/// Where `latest` decodes a head from: the newest chunk the window touches,
/// alone.
///
/// ```text
/// chunks    [10:00 … 10:59] [11:00 … 11:59] [12:00 … 12:20]
/// window             10:30 ────── 11:10
/// decoded                   [11:00 … 11:59]
/// ```
fn from_the_newest_chunk(chunks: &[Chunk<'_>], window: Window) -> i64 {
    let newest = chunks.iter().rev().find(|chunk| chunk.overlaps(window.from, window.to));
    newest.map_or(window.from, |chunk| chunk.head.start.max(window.from))
}

impl Taken {
    /// Decodes the chunks of its head the call asked for onto `out`, whole,
    /// in the order of their times.
    pub(super) fn decode_head(&self, zstd: &mut Zstd, out: &mut Vec<Point>) -> Result<()> {
        let Some(taken) = &self.head else { return Ok(()) };
        let chunks = head::parse(self.found.id, taken.extent, &taken.bytes)?;
        head::decode(zstd, &chunks, taken.from, self.window.to, out)
    }
}

/// A block's samples onto `out`: its times from its clock, its values from
/// its body.
pub(super) fn decode_block(zstd: &mut Zstd, block: &Block, out: &mut Vec<Point>) -> Result<()> {
    let base = out.len();
    out.resize(base + block.times.count, Point::default());
    let run = &mut out[base..];
    let decoded =
        block.times.fill(run).and_then(|()| values::decode(zstd, block.first, &block.times, &block.body, run));
    if decoded.is_err() {
        out.truncate(base);
    }
    decoded.map_err(|error| error.within(format!("a block from {}", block.times.start)))
}
