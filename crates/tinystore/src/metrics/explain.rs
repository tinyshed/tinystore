//! `explain`: what a `read` or an `aggregate` would spend of each of a
//! query's bounds, and the bound it would stop at. It takes the same
//! snapshot the call would, from the series, their directories and their
//! heads, and leaves every block's payload in the file, so that what a plan
//! says is what the call spends. What the call would answer it counts from
//! the times alone: a block's from its clock, and a head's chunk decoded
//! only where the range or a bucket cuts it.
//!
//! ```text
//! 30 days of 15-second samples, 40 series, every 1h
//!   avg    series 40, decoded 960         whole hours answer from their summaries
//!   read   series 40, decoded 6,912,000   exceeds: samples decoded, 1,048,576
//! ```

use std::collections::{BTreeMap, BTreeSet};

use super::aggregate::Shape;
use super::budget::Budget;
use super::codec::Point;
use super::engine::Engine;
use super::head;
use super::query::Asked;
use super::series::Labels;
use super::snapshot::{Taken, Want};
use crate::compress::frame::Zstd;
use crate::{ErrorKind, Result};

/// What a call would spend of a query's bounds.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Plan {
    /// The series the query finds.
    pub series: u64,
    /// The bytes the call reads from the file: labels, directories, clocks,
    /// heads, and the payloads of the blocks it decodes.
    pub bytes: u64,
    /// The samples the call decodes; a block answered from its summary
    /// decodes none.
    pub decoded: u64,
    /// The bound the call would stop at; none when it fits them all.
    pub exceeds: Option<Exceeds>,
}

/// A bound a call would stop at, as its `limit` error would name it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Exceeds {
    /// `series`, `bytes read`, `samples decoded` or `answer`.
    pub limit: &'static str,
    /// What the call would want of it: every sample it would decode or a
    /// read would answer, or what it had asked for where a plan stops
    /// counting, at the series it finds, the bytes it reads and the buckets
    /// an aggregate answers.
    pub wanted: u64,
    pub bound: u64,
}

impl Engine {
    /// The plan of a read, or of the aggregate of `shape`.
    pub(super) fn explain(&self, asked: &Asked, shape: Option<&Shape>) -> Result<Plan> {
        let mut budget = Budget::planning();
        let want = shape.map_or(Want::Samples, Want::Buckets);
        match self.take(asked, want, &mut budget) {
            Ok(taken) => {
                let checked = |shape: &Shape| taken.iter().try_for_each(|series| shape.check(&series.found));
                shape.map_or(Ok(()), checked)?;
                // the call counts its answer once it has decoded: past an earlier bound it never does
                if budget.passed().is_none() {
                    let mut zstd = self.codecs.take();
                    let answered = answer(&mut zstd, &taken, asked, shape, &mut budget);
                    self.codecs.give_back(zstd);
                    answered?;
                }
            }
            // the bound the plan stopped at is its answer, not its failure
            Err(error) if error.kind() == ErrorKind::Limit && budget.passed().is_some() => {}
            Err(error) => return Err(error),
        }
        let exceeds =
            budget.passed().map(|(limit, wanted, bound)| Exceeds { limit, wanted: wanted as u64, bound: bound as u64 });
        Ok(Plan { series: budget.series as u64, bytes: budget.bytes as u64, decoded: budget.decoded as u64, exceeds })
    }
}

/// Counts against the answer's bound what the call would answer: a read's
/// samples in each series' window, or an aggregate's buckets in each group,
/// up to the first past the bound, since a group's buckets are held to be
/// counted.
fn answer(zstd: &mut Zstd, taken: &[Taken], asked: &Asked, shape: Option<&Shape>, budget: &mut Budget) -> Result<()> {
    let Some(shape) = shape else {
        return taken.iter().try_for_each(|series| budget.take_answer(samples_answered(zstd, series)?));
    };
    let mut groups: BTreeMap<(String, Labels), BTreeSet<i64>> = BTreeMap::new();
    for series in taken {
        let buckets = groups.entry(shape.group_of(&series.found)?).or_default();
        for bucket in buckets_answered(zstd, series, asked, shape)? {
            if buckets.insert(bucket) {
                budget.take_answer(1)?;
            }
            if budget.passed().is_some() {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// The samples of a series in its window: a block or a head's chunk inside
/// it counted whole, one the window cuts by its times.
fn samples_answered(zstd: &mut Zstd, series: &Taken) -> Result<usize> {
    let window = series.window;
    let within = |at: i64| at >= window.from && at < window.to;
    let mut count = 0;
    for part in &series.blocks {
        let times = &part.block.times;
        if within(times.start) && within(times.end) {
            count += times.count;
        } else {
            times.walk(|_, at| count += usize::from(within(at)))?;
        }
    }
    let mut points = Vec::new();
    for chunk in series.head_chunks()? {
        if within(chunk.head.start) && within(chunk.head.end) {
            count += chunk.head.count;
            continue;
        }
        decode_chunk(zstd, chunk, &mut points)?;
        count += points.iter().filter(|point| within(point.at)).count();
    }
    Ok(count)
}

/// Where each bucket a series' samples fall in begins, as an aggregate's
/// fold opens them: a block answered whole in the bucket of its first
/// sample, and a run inside the window and one bucket in that bucket; any
/// other by the times of its samples in the window.
fn buckets_answered(zstd: &mut Zstd, series: &Taken, asked: &Asked, shape: &Shape) -> Result<Vec<i64>> {
    let window = series.window;
    let range_from = if asked.from == i64::MIN { window.cutoff } else { asked.from };
    let within = |at: i64| at >= window.from && at < window.to;
    let bucket = |at: i64| shape.bucket_of(at, range_from);
    let one_bucket = |start: i64, end: i64| within(start) && within(end) && bucket(start) == bucket(end);
    let mut buckets = Vec::new();
    let mut note = |at: i64| {
        if within(at) && buckets.last() != Some(&bucket(at)) {
            buckets.push(bucket(at));
        }
    };
    for part in &series.blocks {
        let times = &part.block.times;
        // a block answered whole before the window, where a first step begins, opens no bucket
        if part.whole || one_bucket(times.start, times.end) {
            note(times.start);
        } else {
            times.walk(|_, at| note(at))?;
        }
    }
    let mut points = Vec::new();
    for chunk in series.head_chunks()? {
        if one_bucket(chunk.head.start, chunk.head.end) {
            note(chunk.head.start);
            continue;
        }
        decode_chunk(zstd, chunk, &mut points)?;
        points.iter().for_each(|point| note(point.at));
    }
    Ok(buckets)
}

fn decode_chunk(zstd: &mut Zstd, chunk: head::Chunk<'_>, points: &mut Vec<Point>) -> Result<()> {
    points.clear();
    head::decode(zstd, &[chunk], i64::MIN, i64::MAX, points)
}

#[cfg(test)]
#[path = "explain_tests.rs"]
mod tests;
