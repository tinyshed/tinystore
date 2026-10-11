//! `explain`: what a `read` or an `aggregate` would spend of each of a
//! query's bounds, and the bound it would stop at. It takes the same
//! snapshot the call would, from the series, their directories and their
//! heads, leaves every block's payload in the file and decodes no sample, so
//! that what a plan says is what the call spends.
//!
//! ```text
//! 30 days of 15-second samples, 40 series, every 1h
//!   avg    series 40, decoded 960         whole hours answer from their summaries
//!   read   series 40, decoded 6,912,000   exceeds: samples decoded, 1,048,576
//! ```

use super::aggregate::Shape;
use super::budget::Budget;
use super::engine::Engine;
use super::query::Asked;
use super::snapshot::Want;
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
    /// `series`, `bytes read` or `samples decoded`.
    pub limit: &'static str,
    /// What the call would want of it: every sample it would decode, or
    /// what it had asked for where a plan stops counting, at the series it
    /// finds and the bytes it reads.
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

#[cfg(test)]
#[path = "explain_tests.rs"]
mod tests;
