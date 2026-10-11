//! `read`: the samples of each series a query finds, in its range.
//!
//! ```text
//! find the series ─► copy their heads' and blocks' bytes ─► reserve the store's memory ─► decode ─► answer
//! └──────────── one read of the file ────────────┘   └────────────────── after it ──────────────────┘
//! ```
//!
//! A read transaction covers copying bytes, never decoding them, so that a
//! heavy query is not itself a long reader: the write-ahead log's size is the
//! age of the oldest one.

use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use super::budget::Budget;
use super::codec::Point;
use super::engine::Engine;
use super::head::{self, Extent};
use super::matching::{self, Found};
use super::names::{KEEP, millis};
use super::query::Asked;
use super::series::{Sample, Series};
use crate::clock::from_unix_millis;
use crate::compress::frame::Zstd;
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// What a read reserves of the store's memory for each sample it decodes,
/// beside the bytes it copied.
const SAMPLE_HELD: u64 = 32;
/// How long a read waits for the store's memory.
const PATIENCE: Duration = Duration::from_secs(10);

/// A series as one snapshot of the file held it: where its window starts,
/// and its head's bytes when its range touches them.
#[derive(Debug)]
pub(super) struct Taken {
    pub(super) found: Found,
    /// Where its samples count from: the query's start, or the oldest time
    /// its name keeps, whichever is later.
    pub(super) from: i64,
    pub(super) head: Option<(Extent, Vec<u8>)>,
}

/// A head's extent and its bytes, when the range touches it and the bytes
/// fit what the read may still copy.
const HEAD: &str = "select s.head_count, s.head_start, s.head_end, length(h.body),
    case when s.head_end >= ?2 and s.head_start < ?3 and length(h.body) <= ?4 then h.body end
    from series_state s left join heads h on h.series_id = s.series_id where s.series_id = ?1";

impl Engine {
    pub(super) fn read(&self, asked: &Asked) -> Result<Vec<Series>> {
        let mut budget = Budget::default();
        let taken = self.take(asked, &mut budget)?;
        // every sample of every head taken, which a range may cut to fewer
        let samples: usize =
            taken.iter().filter_map(|series| series.head.as_ref()).map(|(extent, _)| extent.count).sum();
        let weight = budget.bytes as u64 + samples as u64 * SAMPLE_HELD;
        let _held = self.memory.reserve(weight, "metrics: a read", PATIENCE)?;
        let mut zstd = self.codecs.take();
        let answer = answer(&mut zstd, taken, asked, &mut budget);
        self.codecs.give_back(zstd);
        answer
    }

    /// Copies what a query needs out of one snapshot of the file: the series
    /// it finds, and the bytes their ranges touch.
    pub(super) fn take(&self, asked: &Asked, budget: &mut Budget) -> Result<Vec<Taken>> {
        self.file.read(|connection| {
            let found = matching::find(connection, asked, budget)?;
            found.into_iter().map(|series| self.take_series(connection, series, asked, budget)).collect()
        })
    }

    fn take_series(&self, connection: &Connection, found: Found, asked: &Asked, budget: &mut Budget) -> Result<Taken> {
        let keep = self.names.get(found.kept.name()).map_or(KEEP, |name| name.keeps());
        let from = asked.from.max(asked.now.saturating_sub(millis(keep)));
        let failed = |error| sql_error("read a series' head", error);
        let row = connection
            .prepare_cached(HEAD)
            .and_then(|mut select| {
                select
                    .query_row(params![found.id, from, asked.to, budget.bytes_left() as i64], |row| {
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
        let touched = extent.count > 0 && extent.end >= from && extent.start < asked.to;
        if !touched {
            return Ok(Taken { found, from, head: None });
        }
        budget.take_bytes(usize::try_from(length.unwrap_or(0)).unwrap_or(usize::MAX))?;
        let bytes = bytes.ok_or_else(|| Error::corrupt(format!("{}: a head its state says it has", found.kept)))?;
        Ok(Taken { found, from, head: Some((extent, bytes)) })
    }
}

/// Decodes what a snapshot took and keeps each series' samples in its window,
/// in the order of their times; a series without one is left out.
fn answer(zstd: &mut Zstd, taken: Vec<Taken>, asked: &Asked, budget: &mut Budget) -> Result<Vec<Series>> {
    let mut answer = Vec::new();
    let mut points = Vec::new();
    for series in taken {
        points.clear();
        series.decode(zstd, asked.to, budget, &mut points).map_err(|error| error.within(&series.found.kept))?;
        let within: Vec<Sample> = points
            .iter()
            .filter(|point| point.at >= series.from && point.at < asked.to)
            .map(|point| Sample { at: from_unix_millis(point.at), value: point.value })
            .collect();
        if within.is_empty() {
            continue;
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
    /// Decodes the head chunks the range up to `to` touches, paying for
    /// their samples before decoding any.
    fn decode(&self, zstd: &mut Zstd, to: i64, budget: &mut Budget, out: &mut Vec<Point>) -> Result<()> {
        let Some((extent, bytes)) = &self.head else { return Ok(()) };
        let chunks = head::parse(self.found.id, *extent, bytes)?;
        budget.take_decoded(head::samples_within(&chunks, self.from, to))?;
        head::decode(zstd, &chunks, self.from, to, out)
    }
}
