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
//! `groups` compares each group of the series apart, as an aggregate groups
//! them `by` labels, a page at a time with `limit`: a page holds whole
//! groups of as many series as one query finds, so that pages go past that
//! bound a group at a time, and a group of more series than that, alone on
//! its page, is read in parts for every `op` but a percentile.
//!
//! ```no_run
//! # use std::time::Duration;
//! # fn f(stats: tinystore::metrics::Metrics) -> tinystore::Result<()> {
//! use tinystore::metrics::Op;
//! let hosts = stats
//!     .query()
//!     .name("rpc_ms")
//!     .by(&["host"])
//!     .limit(100)
//!     .compare(Op::Avg, Duration::from_mins(15))
//!     .against(Duration::from_hours(24));
//! for host in hosts.groups()?.all() {
//!     let host = host?;
//!     if host.ratio.is_some_and(|ratio| ratio > 2.0) {
//!         println!("{} is slow: {:?} ms against {:?}", host.labels["host"], host.value, host.usual);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! The windows, `now` read once from the store's clock, `w` the window and
//! `last` what `against` reaches back:
//!
//! ```text
//! newest        [now − w, now + 1ms)
//! same_hour     [now − k·24h − w, now − k·24h)    k = 1 … last / 24h
//! otherwise     [now − (k+1)·w, now − k·w)        k = 1 … last / w − 1
//! ```
//!
//! The newest window holds the call's own millisecond, as a read `since` the
//! window does. A page after the first keeps the first page's clock, so that
//! every page compares the same windows.
//!
//! A day is 24 hours, in no time zone: the week after a change of the clock
//! compares with hours one off from the local ones.

use std::time::Duration;

use super::aggregate::{Aggregate, Bucket, Op, Value};
use super::page::ComparePage;
use super::query::{Checked, Query};
use super::series::Labels;
use crate::{Error, Result};

/// The windows `compare` stands its usual on at most. It has no pages: a
/// median of more windows is another question, which a wider window or a
/// shorter `against` asks.
const MOST_WINDOWS: i64 = 1000;
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

/// What `groups` found of one group: its name and the labels the query
/// kept, and what `run` finds of a group, each field as [`Compared`]'s.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ComparedGroup {
    pub name: String,
    /// The labels `by` kept or `without` left; none when neither was given.
    pub labels: Labels,
    pub value: Option<f64>,
    pub usual: Option<f64>,
    pub ratio: Option<f64>,
    pub samples: u64,
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
    /// sample is left out of the median, and so is a partial one, as an
    /// aggregate marks its bucket: one retention cut short, or a `rate`'s or
    /// an `increase`'s whose step into its first sample began at a sample
    /// retention took or after a NaN. A NaN sample in any window is
    /// `invalid`, as in an aggregate, unless the query
    /// [`skip_nan`](Query::skip_nan)s, so no NaN reaches the median.
    ///
    /// Each window reads the blocks it reaches on its own: a block that
    /// spans several windows is decoded, and counted against the query's
    /// bounds, once for each, so minute windows over blocks of hours cost
    /// dozens of times one aggregate of the span. `same_hour`'s windows,
    /// a day apart, read a block about once.
    ///
    /// A timer's percentile is a number only with the query's
    /// [`estimate`](Query::estimate), as `aggregate` gives one; without it
    /// that `op` is `invalid`, since the bounds it lies between are no
    /// number to compare.
    ///
    /// More than 1000 earlier windows is `invalid`, with no pages: a wider
    /// window or a shorter `against` asks for fewer. Past a bound of a
    /// query it is `limit`, whose way on for
    /// series is [`groups`](Compare::groups) with `limit`, where a group of
    /// any count of series is read in parts.
    pub fn run(&self) -> Result<Compared> {
        self.checked_run().and_then(|run| run())
    }

    /// `run` in its two steps, as [`Query::checked_read`] is `read`.
    pub(crate) fn checked_run(&self) -> Result<Checked<Compared>> {
        self.query.checked_compare(self.op, self.windows()?)
    }

    /// What `run` finds, for each group of the query's series apart: those
    /// of a name that share the labels `by` keeps, or every label but those
    /// `without` leaves, in the order of their names and labels. A group
    /// without a sample in any window is left out. Without `limit` it is one
    /// page, whole, from one snapshot, and past a bound it is `limit`. With
    /// `limit` a page holds that many groups at most, whole, of as many
    /// series as one query finds together, each from one snapshot, and gives
    /// `next` while there is more; it stops before a group that would pass a
    /// bound, and a group that alone passes one is `limit`. A group of more
    /// series than one query finds is alone on its page and read in parts,
    /// each from a snapshot of its own, for every `op` but a percentile,
    /// which ranks every series of its group at once: such a group is
    /// `limit`, and the groups before it are not.
    pub fn groups(&self) -> Result<ComparePage> {
        self.checked_groups().and_then(|groups| groups())
    }

    /// `groups` in its two steps, as [`Query::checked_read`] is `read`.
    pub(crate) fn checked_groups(&self) -> Result<Checked<ComparePage>> {
        self.query.checked_compare_groups(self.clone(), self.op, self.windows()?)
    }

    /// The comparison the page after one reads, from the page's `next`.
    pub(crate) fn after(&self, next: &str) -> Compare {
        Compare { query: self.query.clone().after(next), ..self.clone() }
    }

    fn windows(&self) -> Result<Windows> {
        Windows::of(self.window, self.against, self.same_hour).map_err(|error| error.within("metrics"))
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

    /// Every window as `[from, to)`, the newest first, to `now` and its
    /// millisecond, then the earlier ones from the newest to the oldest.
    pub(super) fn ranges(&self, now: i64) -> Vec<(i64, i64)> {
        let step = if self.same_hour { DAY } else { self.window };
        (0..=self.count())
            .map(|k| {
                let to = now.saturating_sub(k.saturating_mul(step));
                let end = if k == 0 { to.saturating_add(1) } else { to };
                (to.saturating_sub(self.window), end)
            })
            .collect()
    }
}

/// The comparison of what each window's aggregate answered, the newest
/// first, when it answered one group; `estimate` says whether a timer's
/// percentile gives its estimate.
pub(super) fn compared(windows: Vec<Vec<Aggregate>>, estimate: bool) -> Result<Compared> {
    let buckets = windows.into_iter().map(one).collect::<Result<Vec<_>>>()?;
    compared_buckets(buckets, estimate)
}

/// The comparison of each window's one bucket, the newest first, none where
/// a window held no sample.
pub(super) fn compared_buckets(windows: Vec<Option<Bucket>>, estimate: bool) -> Result<Compared> {
    let mut windows = windows.into_iter();
    let newest = windows.next().flatten().map(|bucket| valued(bucket, estimate)).transpose()?.flatten();
    let mut earlier = Vec::new();
    for bucket in windows.flatten() {
        let Some((value, _, partial)) = valued(bucket, estimate)? else { continue };
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

impl ComparedGroup {
    pub(super) fn of((name, labels): (String, Labels), compared: Compared) -> ComparedGroup {
        let Compared { value, usual, ratio, samples, windows } = compared;
        ComparedGroup { name, labels, value, usual, ratio, samples, windows }
    }
}

/// A window's one bucket of its one group, when it held a sample.
fn one(groups: Vec<Aggregate>) -> Result<Option<Bucket>> {
    if groups.len() > 1 {
        return Err(Error::invalid(
            "compare answers one group of series, and this query finds several names: name one",
        ));
    }
    Ok(groups.into_iter().next().and_then(|group| group.buckets.into_iter().next()))
}

/// A window's value, its samples and whether it is partial: none for a
/// bucket without a value.
fn valued(bucket: Bucket, estimate: bool) -> Result<Option<(f64, u64, bool)>> {
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
