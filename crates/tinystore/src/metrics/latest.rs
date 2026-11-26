//! `latest`: the newest sample of each series a query finds, when it has one
//! in the range, so that the range bounds how stale an answer may be. It
//! reads one small piece of each series however long the range: a chunk of
//! its head, or its newest block's directory.

use std::time::SystemTime;

use super::budget::Budget;
use super::codec::Point;
use super::engine::Engine;
use super::query::Asked;
use super::read::PATIENCE;
use super::series::Labels;
use super::snapshot::{Taken, Want, decode_block};
use crate::Result;
use crate::clock::from_unix_millis;
use crate::compress::frame::Zstd;

/// A series' newest sample.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Latest {
    pub name: String,
    pub labels: Labels,
    pub at: SystemTime,
    pub value: f64,
}

impl Engine {
    pub(super) fn latest(&self, asked: &Asked) -> Result<Vec<Latest>> {
        let mut budget = Budget::default();
        let taken = self.take(asked, Want::Newest, &mut budget)?;
        self.newest_held(&taken, &mut budget)
    }

    /// `latest` of the series the query finds from the id `from` on, as many
    /// as a query finds, and the id the next part starts from when there are
    /// more: a part a snapshot, so that a name of any count of series is read
    /// whole.
    pub(super) fn latest_from(&self, asked: &Asked, from: i64) -> Result<(Vec<Latest>, Option<i64>)> {
        let mut budget = Budget::default();
        let (taken, next) = self.take_from(asked, Want::Newest, from, &mut budget)?;
        Ok((self.newest_held(&taken, &mut budget)?, next))
    }

    /// The newest sample of each series taken, decoded under the store's
    /// memory for what the snapshot copied.
    fn newest_held(&self, taken: &[Taken], budget: &mut Budget) -> Result<Vec<Latest>> {
        let _held = self.memory.reserve_for_read(budget.bytes as u64, "metrics: the newest samples", PATIENCE)?;
        let mut zstd = self.codecs.take();
        let answer = self.newest_of_each(&mut zstd, taken, budget);
        self.codecs.give_back(zstd);
        answer
    }

    /// Each series' newest sample in its window, in the order a read gives
    /// series; one without a sample there is left out.
    fn newest_of_each(&self, zstd: &mut Zstd, taken: &[Taken], budget: &mut Budget) -> Result<Vec<Latest>> {
        let mut answer = Vec::new();
        let mut points = Vec::new();
        for series in taken {
            let newest = newest(zstd, series, &mut points).map_err(|error| self.met(&series.found, error))?;
            let Some(newest) = newest else { continue };
            budget.take_answer(1)?;
            let kept = &series.found.kept;
            let (name, labels) = (kept.name().to_owned(), kept.labels());
            answer.push(Latest { name, labels, at: from_unix_millis(newest.at), value: newest.value });
        }
        Ok(answer)
    }
}

/// A series' newest sample in its window: its head's when the window touches
/// the head, since every block is older; else its newest block's, from the
/// directory alone when the block ends in the window.
fn newest(zstd: &mut Zstd, series: &Taken, points: &mut Vec<Point>) -> Result<Option<Point>> {
    points.clear();
    match series.blocks.first() {
        Some(part) if part.whole => {
            return Ok(Some(Point { at: part.block.times.end, value: part.block.summary.last }));
        }
        Some(part) => decode_block(zstd, &part.block, points)?,
        None => series.decode_head(zstd, points)?,
    }
    let window = series.window;
    Ok(points.iter().rev().find(|point| point.at >= window.from && point.at < window.to).copied())
}

#[cfg(test)]
#[path = "latest_tests.rs"]
mod tests;
