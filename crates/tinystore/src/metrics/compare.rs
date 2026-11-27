//! `compare`: an aggregate of the newest window set against the usual, the
//! median of the same aggregate over the windows before it.
//!
//! ```no_run
//! # use std::time::Duration;
//! # fn f(stats: tinystore::metrics::Metrics) -> tinystore::Result<()> {
//! use tinystore::metrics::Op;
//! let now = stats
//!     .query()
//!     .name("moodle_call_ms")
//!     .filter("source", "server")
//!     .compare(Op::Percentile(50.0), Duration::from_mins(15))
//!     .against(Duration::from_hours(7 * 24))
//!     .same_hour()
//!     .run()?;
//! if now.ratio.is_some_and(|ratio| ratio > 2.0) {
//!     println!("{:?} ms now against {:?} usually, over {} days", now.value, now.usual, now.windows);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! The windows, `now` read once from the store's clock, `w` the window and
//! `last` what `against` reaches back:
//!
//! ```text
//! newest        [now − w, now)
//! same_hour     [now − k·24h − w, now − k·24h)    k = 1 … last / 24h
//! otherwise     [now − (k+1)·w, now − k·w)        k = 1 … last / w − 1
//! ```
//!
//! A day is 24 hours, in no time zone: the week after a change of the clock
//! compares with hours one off from the local ones.

use std::time::Duration;

use super::aggregate::{Aggregate, Op, Value};
use super::query::{Checked, Query};
use crate::{Error, Result};

/// The windows `compare` stands its usual on at most.
pub(super) const MOST_WINDOWS: i64 = 1000;
const DAY: i64 = 86_400_000;

/// A comparison being asked: the query's series, the `op` of each window,
/// its length, and the windows before it it compares with.
#[derive(Clone, Debug)]
pub struct Compare {
    query: Query,
    op: Op,
    window: Duration,
    against: Option<Duration>,
    same_hour: bool,
}

/// What `compare` found: the newest window's value, the usual one, and what
/// each stands on. A value it could not give is none, never zero.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct Compared {
    /// The `op` over the newest window; none when it holds no sample.
    pub value: Option<f64>,
    /// The median of the `op` over the earlier windows that hold samples;
    /// none when none does.
    pub usual: Option<f64>,
    /// `value / usual`; none when either is none or the usual is zero.
    pub ratio: Option<f64>,
    /// The samples of the newest window; a timer's, its durations.
    pub samples: u64,
    /// The earlier windows the usual stands on.
    pub windows: u32,
}

/// The windows of a comparison, in milliseconds: their length, what they
/// reach back over, and whether they are the same hours of earlier days.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Windows {
    window: i64,
    last: i64,
    same_hour: bool,
}

impl Compare {
    pub(super) fn of(query: Query, op: Op, window: Duration) -> Compare {
        Compare { query, op, window, against: None, same_hour: false }
    }

    /// How far back the windows it compares with reach.
    pub fn against(mut self, last: Duration) -> Self {
        self.against = Some(last);
        self
    }

    /// Compares with the same hours of each earlier day alone, rather than
    /// with every window before the newest.
    pub fn same_hour(mut self) -> Self {
        self.same_hour = true;
        self
    }

    /// The newest window's `op` against the median of the earlier windows'.
    /// Each window is one aggregate of the query's series, merged as an
    /// aggregate without `by` merges them, all from one snapshot of the
    /// file, every bound of a query on them together. A window without a
    /// sample is left out of the median, and so is one retention cut short.
    /// A timer's percentile is a number only with the query's
    /// [`estimate`](Query::estimate), as `aggregate` gives one; without it
    /// that `op` is `invalid`, since the bounds it lies between are no
    /// number to compare.
    pub fn run(&self) -> Result<Compared> {
        self.checked_run().and_then(|run| run())
    }

    /// `run` in its two steps, as [`Query::checked_read`] is `read`.
    pub(crate) fn checked_run(&self) -> Result<Checked<Compared>> {
        let windows =
            Windows::of(self.window, self.against, self.same_hour).map_err(|error| error.within("metrics"))?;
        self.query.checked_compare(self.op, windows)
    }
}

impl Windows {
    fn of(window: Duration, against: Option<Duration>, same_hour: bool) -> Result<Windows> {
        if window.is_zero() || !window.subsec_nanos().is_multiple_of(1_000_000) {
            return Err(Error::invalid("compare: a window of whole milliseconds, one at least"));
        }
        let Some(last) = against else {
            return Err(Error::invalid("compare: `against` says how far back the windows it compares with reach"));
        };
        let (window, last) = (crate::clock::millis(window), crate::clock::millis(last));
        let windows = Windows { window, last, same_hour };
        let count = windows.count();
        if same_hour && window > DAY {
            return Err(Error::invalid("compare: `same_hour` takes a window of a day at most"));
        }
        if count < 1 {
            let least = if same_hour { "a day" } else { "two windows" };
            return Err(Error::invalid(format!("compare: `against` reaches back {least} at least")));
        }
        if count > MOST_WINDOWS {
            return Err(Error::invalid(format!(
                "compare: {count} windows to compare with, {MOST_WINDOWS} at most: a wider window, or a shorter \
                 `against`"
            )));
        }
        Ok(windows)
    }

    /// The earlier windows compared with.
    fn count(&self) -> i64 {
        if self.same_hour { self.last / DAY } else { self.last / self.window - 1 }
    }

    /// Every window as `[from, to)`, the newest first, then the earlier ones
    /// from the newest to the oldest.
    pub(super) fn ranges(&self, now: i64) -> Vec<(i64, i64)> {
        let step = if self.same_hour { DAY } else { self.window };
        (0..=self.count())
            .map(|k| {
                let to = now.saturating_sub(k.saturating_mul(step));
                (to.saturating_sub(self.window), to)
            })
            .collect()
    }
}

/// The comparison of what each window's aggregate answered, the newest
/// first; `estimate` says whether a timer's percentile gives its estimate.
pub(super) fn compared(windows: Vec<Vec<Aggregate>>, estimate: bool) -> Result<Compared> {
    let mut windows = windows.into_iter();
    let newest = windows.next().map(|groups| one(groups, estimate)).transpose()?.flatten();
    let mut earlier = Vec::new();
    for groups in windows {
        let Some((value, _, partial)) = one(groups, estimate)? else { continue };
        if !partial {
            earlier.push(value);
        }
    }
    let usual = median(&mut earlier);
    let value = newest.map(|(value, _, _)| value);
    let ratio = value.zip(usual).filter(|(_, usual)| *usual != 0.0).map(|(value, usual)| value / usual);
    let samples = newest.map_or(0, |(_, samples, _)| samples);
    Ok(Compared { value, usual, ratio, samples, windows: earlier.len() as u32 })
}

/// A window's value, its samples and whether retention cut it, when it held
/// a sample: the one bucket of its one group.
fn one(groups: Vec<Aggregate>, estimate: bool) -> Result<Option<(f64, u64, bool)>> {
    if groups.len() > 1 {
        return Err(Error::invalid(
            "compare answers one group of series, and this query finds several names: name one",
        ));
    }
    let Some(bucket) = groups.into_iter().next().and_then(|group| group.buckets.into_iter().next()) else {
        return Ok(None);
    };
    let value = match bucket.value {
        Value::Exact(value) => value,
        Value::Empty => return Ok(None),
        Value::Between { estimate: Some(point), .. } if estimate => point,
        Value::Between { lower, upper, .. } if estimate => {
            return Err(Error::invalid(format!(
                "compare: a timer's percentile below its first bound, between {lower} and {upper}, has no estimate"
            )));
        }
        Value::Between { lower, upper, .. } => {
            return Err(Error::invalid(format!(
                "compare: a timer's percentile lies between {lower} and {upper}, no one number: ask `estimate` for \
                 a point between them"
            )));
        }
    };
    Ok(Some((value, bucket.samples, bucket.partial)))
}

/// The middle value, or the mean of the two middle ones; none of no values.
fn median(values: &mut [f64]) -> Option<f64> {
    values.sort_unstable_by(f64::total_cmp);
    let middle = values.len() / 2;
    match values.len() {
        0 => None,
        length if !length.is_multiple_of(2) => Some(values[middle]),
        _ => Some((values[middle - 1] + values[middle]) / 2.0),
    }
}

#[cfg(test)]
#[path = "compare_tests.rs"]
mod tests;
