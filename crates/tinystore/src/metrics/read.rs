//! `read`: the samples of each series a query finds, in its range.
//!
//! ```text
//! find the series ─► copy their directories, clocks, payloads and heads ─► reserve the store's memory ─► decode
//! └────────────────── one read of the file ────────────────┘   └────────────── after it ──────────────┘
//! ```
//!
//! A read transaction covers copying bytes, never decoding samples, so that a
//! heavy query is not itself a long reader: the write-ahead log's size is the
//! age of the oldest one. Every row's bytes are paid for before they are
//! copied.

use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use super::budget::Budget;
use super::clock::{self, Times};
use super::codec::Point;
use super::directory::{Block, Group, Row};
use super::engine::Engine;
use super::head::{self, Extent};
use super::matching::{self, Found};
use super::names::{KEEP, millis};
use super::query::Asked;
use super::series::{Sample, Series};
use super::values;
use crate::clock::from_unix_millis;
use crate::compress::frame::Zstd;
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// What a read reserves of the store's memory for each sample it decodes,
/// beside the bytes it copied.
const SAMPLE_HELD: u64 = 32;
/// How long a read waits for the store's memory.
const PATIENCE: Duration = Duration::from_secs(10);
/// The decoded clocks a read keeps, since series written together share
/// theirs.
const KEPT_CLOCKS: usize = 8;

/// A series as one snapshot of the file held it.
#[derive(Debug)]
pub(super) struct Taken {
    pub(super) found: Found,
    /// Where its samples count from: the query's start, or the oldest time
    /// its name keeps, whichever is later.
    pub(super) from: i64,
    /// Its live blocks the range touches, in the order of their times, their
    /// bodies copied.
    pub(super) blocks: Vec<Block>,
    pub(super) head: Option<(Extent, Vec<u8>)>,
}

/// A head's extent and its bytes, when the range touches it and the bytes
/// fit what the read may still copy.
const HEAD: &str = "select s.head_count, s.head_start, s.head_end, length(h.body),
    case when s.head_end >= ?2 and s.head_start < ?3 and length(h.body) <= ?4 then h.body end
    from series_state s left join heads h on h.series_id = s.series_id where s.series_id = ?1";
/// The groups that may hold samples of `[?2, ?3)`: from the last that starts
/// at or before `?2`, so that the index walks no older one.
const GROUPS: &str = "select start_ts, end_ts, clock, length(directory),
    case when length(directory) <= ?4 then directory end
    from groups where series_id = ?1
    and start_ts >= coalesce((select start_ts from groups where series_id = ?1 and start_ts <= ?2
        order by start_ts desc limit 1), ?2)
    and start_ts < ?3 and end_ts >= ?2 order by start_ts";
const PAYLOAD: &str = "select body from payloads where id = ?1";

impl Engine {
    pub(super) fn read(&self, asked: &Asked) -> Result<Vec<Series>> {
        let mut budget = Budget::default();
        let taken = self.take(asked, &mut budget)?;
        let weight = budget.bytes as u64 + budget.decoded as u64 * SAMPLE_HELD;
        let _held = self.memory.reserve(weight, "metrics: a read", PATIENCE)?;
        let mut zstd = self.codecs.take();
        let answer = answer(&mut zstd, taken, asked, &mut budget);
        self.codecs.give_back(zstd);
        answer
    }

    /// Copies what a query needs out of one snapshot of the file: the series
    /// it finds, and the bytes their ranges touch.
    pub(super) fn take(&self, asked: &Asked, budget: &mut Budget) -> Result<Vec<Taken>> {
        let mut zstd = self.codecs.take();
        let taken = self.file.read(|connection| {
            let found = matching::find(connection, asked, budget)?;
            let mut reading = Reading { connection, zstd: &mut zstd, budget, clocks: Vec::new() };
            found.into_iter().map(|series| reading.series(self, series, asked)).collect()
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

impl Reading<'_, '_> {
    fn series(&mut self, engine: &Engine, found: Found, asked: &Asked) -> Result<Taken> {
        let keep = engine.names.get(found.kept.name()).map_or(KEEP, |name| name.keeps());
        let from = asked.from.max(asked.now.saturating_sub(millis(keep)));
        let blocks = self.blocks(found.id, from, asked.to).map_err(|error| error.within(&found.kept))?;
        let head = self.head(&found, from, asked.to)?;
        Ok(Taken { found, from, blocks, head })
    }

    /// The live blocks of a series that `[from, to)` touches, each paid for:
    /// its samples, and its payload's bytes before they are copied.
    fn blocks(&mut self, id: i64, from: i64, to: i64) -> Result<Vec<Block>> {
        let rows = self.group_rows(id, from, to)?;
        let mut blocks = Vec::new();
        for row in rows {
            let clock = self.clock(row.clock)?;
            let group = Group::decode(self.zstd, &row, clock)?;
            for (slot, mut block) in group.blocks.into_iter().enumerate() {
                if group.live >> slot & 1 == 0 || block.times.end < from || block.times.start >= to {
                    continue;
                }
                self.budget.take_decoded(block.times.count)?;
                if block.external() {
                    self.budget.take_bytes(block.body_len)?;
                    block.body = self.payload(block.payload, block.body_len)?;
                }
                blocks.push(block);
            }
        }
        Ok(blocks)
    }

    fn group_rows(&mut self, id: i64, from: i64, to: i64) -> Result<Vec<Row>> {
        let failed = |error| sql_error("read a series' groups", error);
        let mut select = self.connection.prepare_cached(GROUPS).map_err(failed)?;
        let room = self.budget.bytes_left() as i64;
        let mut rows = select.query(params![id, from, to, room]).map_err(failed)?;
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

    /// A clock this read decoded, or the file's, paid for.
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

    /// A series' head when `[from, to)` touches it, its bytes and the samples
    /// of the chunks the range touches paid for.
    fn head(&mut self, found: &Found, from: i64, to: i64) -> Result<Option<(Extent, Vec<u8>)>> {
        let failed = |error| sql_error("read a series' head", error);
        let room = self.budget.bytes_left() as i64;
        let row = self
            .connection
            .prepare_cached(HEAD)
            .and_then(|mut select| {
                select
                    .query_row(params![found.id, from, to, room], |row| {
                        let extent =
                            (row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?, row.get::<_, Option<i64>>(2)?);
                        Ok((extent, row.get::<_, Option<i64>>(3)?, row.get::<_, Option<Vec<u8>>>(4)?))
                    })
                    .optional()
            })
            .map_err(failed)?;
        let Some(((count, start, end), length, bytes)) = row else {
            return Err(Error::corrupt(format!("{}: a series without its state", found.kept)));
        };
        let extent =
            Extent { count: usize::try_from(count).unwrap_or(0), start: start.unwrap_or(0), end: end.unwrap_or(0) };
        if extent.count == 0 || extent.end < from || extent.start >= to {
            return Ok(None);
        }
        self.budget.take_bytes(usize::try_from(length.unwrap_or(0)).unwrap_or(usize::MAX))?;
        let bytes = bytes.ok_or_else(|| Error::corrupt(format!("{}: a head its state says it has", found.kept)))?;
        let chunks = head::parse(found.id, extent, &bytes).map_err(|error| error.within(&found.kept))?;
        self.budget.take_decoded(head::samples_within(&chunks, from, to))?;
        Ok(Some((extent, bytes)))
    }
}

/// Decodes what a snapshot took and keeps each series' samples in its window,
/// in the order of their times; a series without one is left out.
fn answer(zstd: &mut Zstd, taken: Vec<Taken>, asked: &Asked, budget: &mut Budget) -> Result<Vec<Series>> {
    let mut answer = Vec::new();
    let mut points = Vec::new();
    for series in taken {
        points.clear();
        series.decode(zstd, asked.to, &mut points).map_err(|error| error.within(&series.found.kept))?;
        let within: Vec<Sample> = points
            .iter()
            .filter(|point| point.at >= series.from && point.at < asked.to)
            .map(|point| Sample { at: from_unix_millis(point.at), value: point.value })
            .collect();
        if within.is_empty() {
            continue;
        }
        if within.windows(2).any(|pair| pair[1].at <= pair[0].at) {
            return Err(Error::corrupt(format!("{}: samples out of the order of their times", series.found.kept)));
        }
        budget.take_answer(within.len())?;
        let kept = &series.found.kept;
        let mut read = Series::of(kept.name().to_owned(), series.found.kind);
        read.labels = kept.labels();
        read.samples = within;
        answer.push(read);
    }
    Ok(answer)
}

impl Taken {
    /// Decodes its blocks and then the head chunks the range up to `to`
    /// touches, in the order of their times.
    pub(super) fn decode(&self, zstd: &mut Zstd, to: i64, out: &mut Vec<Point>) -> Result<()> {
        for block in &self.blocks {
            decode_block(zstd, block, out)?;
        }
        let Some((extent, bytes)) = &self.head else { return Ok(()) };
        let chunks = head::parse(self.found.id, *extent, bytes)?;
        head::decode(zstd, &chunks, self.from, to, out)
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
