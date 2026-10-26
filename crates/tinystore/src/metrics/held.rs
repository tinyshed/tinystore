//! An aggregate of series a program holds, with no store: samples it reads
//! live and never keeps, counted by the code a query's are, so that a panel
//! over them and one over the same samples in a store agree to the bit.
//!
//! ```
//! # use std::time::{Duration, UNIX_EPOCH};
//! # fn main() -> tinystore::Result<()> {
//! use tinystore::metrics::{Op, Series, Value};
//! let at = |s: u64| UNIX_EPOCH + Duration::from_secs(1_800_000_000 + s);
//! let requests = [100.0, 150.0, 200.0, 20.0, 70.0].iter().enumerate();
//! let requests = requests.fold(Series::counter("requests"), |series, (i, v)| series.sample(at(i as u64 * 10), *v));
//! let minute = requests.query().from(at(0)).to(at(60)).aggregate(Op::Increase)?;
//! assert_eq!((minute.buckets[0].value, minute.buckets[0].resets), (Value::Exact(170.0), 1));
//! # Ok(())
//! # }
//! ```
//!
//! A group is the series given together, which share a name and a kind: each
//! is counted exactly, then joined, and the group's value rounded once. Which
//! series make a group is the program's to say; the labels of the answer are
//! those every one of them has.

use std::time::{Duration, SystemTime};

use super::aggregate::{Aggregate, Asking, Op, Shape};
use super::codec::Point;
use super::labels::{BIN, Kept};
use super::matching::Found;
use super::query::Asked;
use super::series::{Kind, Labels, Series};
use super::snapshot::Window;
use crate::calendar::Calendar;
use crate::{Error, Result, unix_millis};

impl Series {
    /// An aggregate of this series alone, as a query of the store answers
    /// one for the same samples, and with no store.
    pub fn query(&self) -> SeriesQuery<'_> {
        query(std::slice::from_ref(self))
    }
}

/// An aggregate of the series a program holds, one group, as a query of the
/// store answers one for the same samples: `series` share a name and a kind.
pub fn query(series: &[Series]) -> SeriesQuery<'_> {
    SeriesQuery { series, from: None, to: None, every: None, calendar: None, estimate: false }
}

/// An aggregate of series a program holds, its range and its buckets as a
/// store's [`Query`](super::Query) says them. Without `from` and `to` the
/// range is the samples', from the first to the last.
#[derive(Clone, Debug)]
#[must_use = "a query answers by aggregate"]
pub struct SeriesQuery<'a> {
    series: &'a [Series],
    from: Option<SystemTime>,
    to: Option<SystemTime>,
    every: Option<Duration>,
    calendar: Option<(Calendar, String)>,
    estimate: bool,
}

impl SeriesQuery<'_> {
    /// The range from this time on; a sample before it is where a step into
    /// the first bucket begins, as in a store's query.
    pub fn from(mut self, from: SystemTime) -> Self {
        self.from = Some(from);
        self
    }

    /// The range until this time, not at it.
    pub fn to(mut self, to: SystemTime) -> Self {
        self.to = Some(to);
        self
    }

    /// Buckets of this span, counted from 1970 in UTC; without them the
    /// range is one bucket.
    pub fn every(mut self, span: Duration) -> Self {
        self.every = Some(span);
        self
    }

    /// Buckets of a calendar's days, weeks or months in the time zone of its
    /// IANA name, as long as each is there.
    pub fn calendar(mut self, unit: Calendar, zone: &str) -> Self {
        self.calendar = Some((unit, zone.to_owned()));
        self
    }

    /// A timer's or a histogram's percentile gives a point between its
    /// bounds too, as Prometheus' `histogram_quantile` interpolates one.
    pub fn estimate(mut self) -> Self {
        self.estimate = true;
        self
    }

    /// The group's buckets, each one exact value of `op`, rounded once, with
    /// `samples`, `resets`, `gap` and `partial` as a query gives them; a
    /// bucket without a sample is left out. An `op` the series' kind has
    /// not is `invalid`, and so is a bucket over a NaN, an infinity or a
    /// counter below zero, or two samples of one series at one millisecond.
    pub fn aggregate(&self, op: Op) -> Result<Aggregate> {
        let calendar = self.calendar.as_ref().map(|(unit, zone)| (*unit, zone.as_str()));
        let asking = Asking { every: self.every, calendar, estimate: self.estimate, ..Asking::default() };
        let shape = Shape::of(op, asking)?;
        let (name, kind) = self.name_and_kind()?;
        let mut members = self.series.iter().enumerate().map(member).collect::<Result<Vec<_>>>()?;
        let asked = self.asked(&members)?;
        let window = Window { from: asked.from, to: asked.to, cutoff: i64::MIN };
        for member in &members {
            shape.check(&member.found)?;
        }
        let labels = shared_labels(self.series, op, kind);
        let buckets = super::counting::count_held(&shape, &mut members, window, &asked)?;
        Ok(Aggregate { name, labels, buckets, more: false })
    }

    /// The one name and kind of the series.
    fn name_and_kind(&self) -> Result<(String, Kind)> {
        let first = self.series.first().ok_or_else(|| Error::invalid("an aggregate of no series"))?;
        if let Some(other) = self.series.iter().find(|series| series.name != first.name || series.kind != first.kind) {
            let (first_kind, other_kind) = (first.kind, other.kind);
            return Err(Error::invalid(format!(
                "a group's series share a name and a kind: {first}, a {first_kind}, and {other}, a {other_kind}"
            )));
        }
        Ok((first.name.clone(), first.kind))
    }

    /// The range, the samples' where it is not given, and the clock read once.
    fn asked(&self, members: &[Held]) -> Result<Asked> {
        let times = members.iter().flat_map(|member| member.points.iter().map(|point| point.at));
        let (first, last) = times.fold((i64::MAX, i64::MIN), |(first, last), at| (first.min(at), last.max(at)));
        let from = self.from.map_or(first, unix_millis);
        let to = self.to.map_or_else(|| last.saturating_add(1).max(from), unix_millis);
        if to < from {
            return Err(Error::invalid("a query whose range ends before it starts"));
        }
        let now = unix_millis(SystemTime::now());
        Ok(Asked { name: None, conditions: Vec::new(), from, to, now })
    }
}

/// A series a program holds, as a query's counting takes one.
pub(super) struct Held {
    pub(super) found: Found,
    /// Its samples in the order of their times, each time once.
    pub(super) points: Vec<Point>,
}

fn member((at, series): (usize, &Series)) -> Result<Held> {
    let kept = Kept::held(&series.name, &series.labels).map_err(|error| error.within(series))?;
    let mut points: Vec<Point> =
        series.samples.iter().map(|sample| Point { at: unix_millis(sample.at), value: sample.value }).collect();
    points.sort_by_key(|point| point.at);
    // a sample given twice, bit for bit, is one sample, as an ingest sent again is
    points.dedup_by(|later, earlier| later.at == earlier.at && later.value.to_bits() == earlier.value.to_bits());
    if let Some(pair) = points.windows(2).find(|pair| pair[0].at == pair[1].at) {
        let at = super::timestamp::rfc3339(pair[0].at);
        return Err(
            Error::invalid(format!("two samples at {at}, one millisecond, which a series has one of")).within(series)
        );
    }
    Ok(Held { found: Found { id: at as i64, kind: series.kind, kept }, points })
}

/// The labels every series has with one value, a histogram's `le` aside
/// where a percentile ranks its buckets together.
fn shared_labels(series: &[Series], op: Op, kind: Kind) -> Labels {
    let mut shared = series.first().map(|series| series.labels.clone()).unwrap_or_default();
    shared.retain(|label, value| series.iter().all(|series| series.labels.get(label) == Some(value)));
    if matches!(op, Op::Percentile(_)) && kind == Kind::Histogram {
        shared.remove("le");
        shared.remove(BIN);
    }
    shared
}

#[cfg(test)]
#[path = "held_tests.rs"]
mod tests;
