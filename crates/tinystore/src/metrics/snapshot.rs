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
use super::page::{Paging, Place};
use super::query::Asked;
use super::timer;
use super::values;
use crate::compress::frame::Zstd;
use crate::sqlite::sql_error;
use crate::{Error, ErrorKind, Result};

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
    /// Where a page stopped taking it: its samples from here on are the next
    /// page's. The window's end when nothing stopped it.
    pub(super) until: i64,
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
const HEAD: &str = "select body from heads where series_id = ?1 and length(body) <= ?2";

impl Engine {
    /// Copies what a call needs out of one snapshot of the file: the series
    /// its query finds, and of each the bytes the call takes.
    pub(super) fn take(&self, asked: &Asked, want: Want<'_>, budget: &mut Budget) -> Result<Vec<Taken>> {
        self.snapshot(asked, want, budget, |reading, asked| {
            let found = matching::find(reading.connection, asked, reading.budget)?;
            found.into_iter().map(|series| reading.series(self, series, asked, want)).collect()
        })
    }

    /// What a page of a read takes, as [`Engine::take`] takes a read's, and
    /// where it stopped: the place the next page begins, none when it took
    /// the rest of the range.
    pub(super) fn take_page(
        &self,
        asked: &Asked,
        paging: &Paging,
        budget: &mut Budget,
    ) -> Result<(Vec<Taken>, Option<Place>)> {
        self.snapshot(asked, Want::Samples, budget, |reading, asked| {
            let (found, beyond) = matching::find_from(reading.connection, asked, paging.from.series, reading.budget)?;
            reading.page(self, found, asked, paging).map(|(taken, stopped)| {
                let past = beyond.map(|series| Place { series, at: i64::MIN });
                (taken, stopped.or(past))
            })
        })
    }

    /// `read` run on one snapshot of the file, the query read as a timer's
    /// series when it names one. A series whose bytes no longer read is
    /// suspended once the snapshot has ended, since a suspension writes.
    fn snapshot<T>(
        &self,
        asked: &Asked,
        want: Want<'_>,
        budget: &mut Budget,
        read: impl FnOnce(&mut Reading<'_, '_>, &Asked) -> Result<T>,
    ) -> Result<T> {
        let mut zstd = self.codecs.take();
        let mut met = None;
        let taken = self.file.read(|connection| {
            let read_as = timer::read_as(connection, asked, want)?;
            let asked = read_as.as_ref().unwrap_or(asked);
            let mut reading = Reading { connection, zstd: &mut zstd, budget, clocks: Vec::new(), met: &mut met };
            read(&mut reading, asked)
        });
        self.codecs.give_back(zstd);
        if let Some((id, failure)) = met {
            // the read's own error is what its caller needs
            self.suspend(id, &failure).ok();
        }
        taken
    }
}

/// One snapshot being copied: its connection, the budget it pays from, the
/// clocks it decoded, and the series whose bytes it met no longer reading.
struct Reading<'r, 'c> {
    connection: &'c Connection,
    zstd: &'r mut Zstd,
    budget: &'r mut Budget,
    clocks: Vec<(i64, Vec<Times>)>,
    met: &'r mut Option<(i64, String)>,
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
        let mut taken = Taken::of(engine, found, asked, i64::MIN);
        match self.fill(&mut taken, asked.now, want) {
            Ok(()) => Ok(taken),
            Err(error) => Err(self.met(&taken.found, error)),
        }
    }

    /// The series of a page in the order of their ids, each taken from its
    /// blocks, oldest first, then its head, for as long as the page has room;
    /// and where it stopped, before the part that would pass `limit` or a
    /// bound of the query.
    fn page(
        &mut self,
        engine: &Engine,
        found: Vec<Found>,
        asked: &Asked,
        paging: &Paging,
    ) -> Result<(Vec<Taken>, Option<Place>)> {
        let mut taken = Vec::with_capacity(found.len());
        for found in found {
            let at = if found.id == paging.from.series { paging.from.at } else { i64::MIN };
            let mut series = Taken::of(engine, found, asked, at);
            let stopped = self.fill_page(&mut series, asked.now, paging.limit);
            let stopped = stopped.map_err(|error| self.met(&series.found, error))?;
            let id = series.found.id;
            taken.push(series);
            if let Some(at) = stopped {
                return Ok((taken, Some(Place { series: id, at })));
            }
        }
        Ok((taken, None))
    }

    /// A series' blocks and head a page takes, and the time it stopped at
    /// when it could not take them all.
    fn fill_page(&mut self, taken: &mut Taken, now: i64, limit: usize) -> Result<Option<i64>> {
        let state = self.state(taken.found.id, taken.window, now, -1)?;
        // every block is older than the head, so a head that ends before the
        // window leaves it nothing
        if state.extent.count > 0 && state.extent.end < taken.window.from {
            return Ok(None);
        }
        let stopped = match self.blocks_page(taken, limit)? {
            Some(at) => Some(at),
            None => self.head_page(taken, &state, limit)?,
        };
        taken.until = stopped.unwrap_or(taken.window.to);
        Ok(stopped)
    }

    /// Whether a page holds as many samples as it may answer: what it
    /// decoded is at least what it answers.
    fn full(&self, limit: usize) -> bool {
        self.budget.decoded >= limit
    }

    /// The live blocks of a series in the window a page takes, a group's
    /// directory and clock read only once the page reaches the group; and the
    /// time of the first block it could not take.
    fn blocks_page(&mut self, taken: &mut Taken, limit: usize) -> Result<Option<i64>> {
        let (id, window) = (taken.found.id, taken.window);
        let connection = self.connection;
        let failed = |error| sql_error("read a series' groups", error);
        let mut select = connection.prepare_cached(GROUPS).map_err(failed)?;
        let room = self.budget.bytes_left() as i64;
        let mut rows = select.query(params![id, window.from, window.to, window.cutoff, room]).map_err(failed)?;
        while let Some(row) = rows.next().map_err(failed)? {
            let start: i64 = row.get(0).map_err(failed)?;
            let length = usize::try_from(row.get::<_, i64>(3).map_err(failed)?).unwrap_or(usize::MAX);
            // a clock is paid at its most, so that one that does not fit never fails the page
            if self.full(limit) || !self.budget.fits(length.saturating_add(clock::MOST_BYTES), 0) {
                return Ok(Some(start.max(window.from)));
            }
            self.budget.take_bytes(length)?;
            let directory: Option<Vec<u8>> = row.get(4).map_err(failed)?;
            let directory = directory.ok_or_else(|| Error::corrupt("a group's directory past its length"))?;
            let (end, clock) = (row.get(1).map_err(failed)?, row.get(2).map_err(failed)?);
            let row = Row { series: id, start, end, clock, directory };
            if let Some(at) = self.group_page(taken, &row, limit)? {
                return Ok(Some(at));
            }
        }
        Ok(None)
    }

    /// The live blocks of one group in the window a page takes, and the
    /// start of the first it could not take.
    fn group_page(&mut self, taken: &mut Taken, row: &Row, limit: usize) -> Result<Option<i64>> {
        let window = taken.window;
        let clock = self.clock(row.clock)?;
        let group = Group::decode(self.zstd, row, clock)?;
        for (slot, block) in group.blocks.into_iter().enumerate() {
            if group.live >> slot & 1 == 0 || block.times.end < window.from || block.times.start >= window.to {
                continue;
            }
            let bytes = if block.external() { block.body_len } else { 0 };
            if self.full(limit) || !self.budget.fits(bytes, block.times.count) {
                return Ok(Some(block.times.start.max(window.from)));
            }
            taken.blocks.push(self.part(block, false)?);
        }
        Ok(None)
    }

    /// A series' head a page takes, chunk by chunk, and the start of the
    /// first chunk it could not take. Its bytes are copied whole, once the
    /// page has room for them.
    fn head_page(&mut self, taken: &mut Taken, state: &State, limit: usize) -> Result<Option<i64>> {
        let (id, extent, window) = (taken.found.id, state.extent, taken.window);
        if extent.count == 0 || extent.end < window.from || extent.start >= window.to {
            return Ok(None);
        }
        if self.full(limit) || !self.budget.fits(state.head_len, 0) {
            return Ok(Some(extent.start.max(window.from)));
        }
        self.budget.take_bytes(state.head_len)?;
        let bytes = self.head_bytes(id, state.head_len)?;
        let mut stopped = None;
        for chunk in head::parse(id, extent, &bytes)?.iter().filter(|chunk| chunk.overlaps(window.from, window.to)) {
            if self.full(limit) || !self.budget.fits(0, chunk.head.count) {
                stopped = Some(chunk.head.start.max(window.from));
                break;
            }
            self.budget.take_decoded(chunk.head.count)?;
        }
        taken.head = Some(TakenHead { extent, bytes, from: window.from });
        Ok(stopped)
    }

    /// A head's bytes, read apart from its state's row for a page, which
    /// copies them only once it reaches them.
    fn head_bytes(&self, id: i64, length: usize) -> Result<Vec<u8>> {
        let asked = params![id, i64::try_from(length).unwrap_or(i64::MAX)];
        let body: Option<Vec<u8>> = self
            .connection
            .prepare_cached(HEAD)
            .and_then(|mut select| select.query_row(asked, |row| row.get(0)).optional())
            .map_err(|error| sql_error("read a series' head", error))?;
        body.filter(|body| body.len() == length).ok_or_else(|| Error::corrupt("a head its state says it has"))
    }

    /// Names an error a series met as the series; when its bytes no longer
    /// read, the series is kept for the suspension the snapshot's end makes.
    fn met(&mut self, found: &Found, error: Error) -> Error {
        if error.kind() == ErrorKind::Corrupt {
            *self.met = Some((found.id, error.to_string()));
        }
        error.within(&found.kept)
    }

    /// A series' state, and the head and blocks of it the call takes.
    fn fill(&mut self, taken: &mut Taken, now: i64, want: Want<'_>) -> Result<()> {
        let (id, window) = (taken.found.id, taken.window);
        let state = self.state(id, window, now, self.budget.bytes_left() as i64)?;
        let extent = state.extent;
        taken.trimmed = state.trimmed;
        // every block is older than the head, so a head that ends before the
        // window leaves it nothing
        if extent.count > 0 && extent.end < window.from {
            return Ok(());
        }
        taken.head = self.head(&taken.found, state, window, want)?;
        taken.blocks = match want {
            Want::Newest if taken.head.is_some() => Vec::new(),
            Want::Newest => self.newest_block(id, window)?,
            Want::Samples => self.blocks(id, window, None, false)?,
            Want::Buckets(shape) => {
                // a head that starts before the range holds the sample a first step begins at
                let in_head = extent.count > 0 && extent.start < window.from;
                let op = shape.series_op(&taken.found);
                let before = Shape::steps(op) && window.from > window.cutoff && !in_head;
                self.blocks(id, window, Some((shape, op)), before)?
            }
        };
        Ok(())
    }

    fn state(&mut self, id: i64, window: Window, now: i64, room: i64) -> Result<State> {
        let row = self.connection.prepare_cached(STATE).and_then(|mut select| {
            let asked = params![id, window.from, window.to, room, now];
            let read = select.query_row(asked, |row| {
                let extent = (row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?, row.get::<_, Option<i64>>(2)?);
                Ok((extent, row.get::<_, bool>(3)?, row.get::<_, Option<i64>>(4)?, row.get::<_, Option<Vec<u8>>>(5)?))
            });
            read.optional()
        });
        let row = row.map_err(|error| sql_error("read a series' state", error))?;
        let Some(((count, start, end), trimmed, length, head)) = row else {
            return Err(Error::corrupt("a series without its state"));
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
        let bytes = state.head.ok_or_else(|| Error::corrupt("a head its state says it has"))?;
        let chunks = head::parse(found.id, extent, &bytes)?;
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
    /// A series to take over the query's window: from `at` on when a page
    /// goes on inside it, and never before the oldest time its name keeps.
    fn of(engine: &Engine, found: Found, asked: &Asked, at: i64) -> Taken {
        let keep = engine.names.get(found.kept.name()).map_or(KEEP, |name| name.keeps());
        let cutoff = asked.now.saturating_sub(millis(keep));
        let window = Window { from: asked.from.max(cutoff).max(at), to: asked.to, cutoff };
        Taken { found, window, trimmed: false, blocks: Vec::new(), head: None, until: window.to }
    }

    /// Decodes the chunks of its head the call asked for onto `out`, whole,
    /// in the order of their times.
    pub(super) fn decode_head(&self, zstd: &mut Zstd, out: &mut Vec<Point>) -> Result<()> {
        let Some(taken) = &self.head else { return Ok(()) };
        let chunks = head::parse(self.found.id, taken.extent, &taken.bytes)?;
        head::decode(zstd, &chunks, taken.from, self.until, out)
    }

    /// The chunks of its head the call decodes.
    pub(super) fn head_chunks(&self) -> Result<Vec<Chunk<'_>>> {
        let Some(taken) = &self.head else { return Ok(Vec::new()) };
        let chunks = head::parse(self.found.id, taken.extent, &taken.bytes)?;
        Ok(chunks.into_iter().filter(|chunk| chunk.overlaps(taken.from, self.window.to)).collect())
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
