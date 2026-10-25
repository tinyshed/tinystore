//! `aggregate`: a query's range cut into buckets of `every`, each one value
//! of an `op`, a group of series a bucket.
//!
//! ```no_run
//! # use std::time::Duration;
//! # fn f(stats: tinystore::metrics::Metrics) -> tinystore::Result<()> {
//! use tinystore::metrics::{Op, Value};
//! let traffic = stats
//!     .query()
//!     .name("http_requests_total")
//!     .since(Duration::from_hours(24))
//!     .every(Duration::from_hours(1))
//!     .by(&["route"])
//!     .aggregate(Op::Increase)?;
//! for route in traffic {
//!     let hours: Vec<Option<f64>> = route.buckets.iter().map(|hour| hour.value.exact()).collect();
//!     println!("{}: {hours:?}", route.labels["route"]);
//! }
//! let p95 = stats.query().name("http_request_ms").since(Duration::from_hours(1)).aggregate(Op::Percentile(95.0))?;
//! if let Value::Between { lower, upper, .. } = p95[0].buckets[0].value {
//!     println!("95% of requests took at most {upper} ms, and the rest more than {lower}");
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Buckets are spans of `every` counted from 1970 in UTC, or of a calendar
//! in a time zone, and the range cuts the first and the last. A series' samples reach its buckets in the order
//! of their times, a block inside one bucket whole, from its summary, and a
//! step between two samples is counted in the bucket it ends in:
//!
//! ```text
//! samples   09:59:45   10:00:00 … 10:59:45   11:00:00 …
//! buckets   ────────── 10:00 ─────────────── 11:00 ──────
//!           carried into 10:00's first step  the step from 10:59:45 counts at 11:00
//! ```

use std::fmt;
use std::time::{Duration, SystemTime};

use super::buckets::le;
use super::directory::Block;
use super::labels::{BIN, MOST_LABELS, MOST_NAME_BYTES, OVERFLOW};
use super::matching::Found;
use super::rank::Rank;
use super::series::{Kind, Labels};
use super::snapshot::Window;
use super::timer::Part;
use crate::calendar::{Calendar, Zoned};
use crate::{Error, Result};

/// What a bucket's value is of its samples.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Op {
    /// How many samples; a timer's, how many durations.
    Count,
    /// A gauge's exact sum; a timer's durations'.
    Sum,
    /// A gauge's sum over its count; a timer's durations'.
    Avg,
    /// A gauge's least sample.
    Min,
    /// A gauge's greatest sample; a timer's longest duration.
    Max,
    /// How much a counter grew, a reset counted as its new value.
    Increase,
    /// A counter's increase a second of the bucket; a timer's durations a
    /// second.
    Rate,
    /// How much a gauge moved: its last sample less the one before its
    /// first.
    Delta,
    /// The first sample, bit for bit; a group adds its series'.
    First,
    /// The last sample, bit for bit; a group adds its series'.
    Last,
    /// The value at a rank above 0 and at most 100, `Percentile(95.0)`: a
    /// gauge's sample at it, the nearest rank, exact; a timer's bucket of its
    /// spread that holds it, [`Value::Between`].
    Percentile(f64),
}

impl Op {
    /// Whether a series of `kind` has it; a histogram's `_bucket` series is
    /// a counter, which a percentile ranks as a timer's spread.
    fn is_of(self, kind: Kind) -> bool {
        match self {
            Op::Count | Op::First | Op::Last => true,
            Op::Sum | Op::Avg | Op::Min | Op::Max | Op::Delta => kind == Kind::Gauge,
            Op::Increase | Op::Rate => kind != Kind::Gauge,
            Op::Percentile(_) => kind != Kind::Counter,
        }
    }
}

impl fmt::Display for Op {
    /// The `op` as the wire and every SDK say it: `increase`, `p99.9`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let word = match self {
            Op::Percentile(rank) if rank.is_finite() && *rank > 0.0 => return write!(f, "p{}", le(*rank)),
            Op::Percentile(rank) => return write!(f, "p{rank}"),
            Op::Count => "count",
            Op::Sum => "sum",
            Op::Avg => "avg",
            Op::Min => "min",
            Op::Max => "max",
            Op::Increase => "increase",
            Op::Rate => "rate",
            Op::Delta => "delta",
            Op::First => "first",
            Op::Last => "last",
        };
        f.write_str(word)
    }
}

/// A group of an aggregate: the series of one name that share the labels the
/// query kept, and their buckets in the order of their times.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Aggregate {
    /// The name, a timer's own when the query named one.
    pub name: String,
    /// The labels `by` kept or `without` left; none when neither was given,
    /// every series of the name one group.
    pub labels: Labels,
    /// A bucket without samples is left out.
    pub buckets: Vec<Bucket>,
}

/// One bucket of an aggregate.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct Bucket {
    /// Where the bucket begins: a whole number of `every` since 1970, or the
    /// range's start when the range is one bucket.
    pub at: SystemTime,
    pub value: Value,
    /// How many samples it comes from; a timer's, how many durations.
    pub samples: u64,
    /// A counter's resets in it; a timer's, its restarts.
    pub resets: u64,
    /// Its first step began before the bucket before it: a whole bucket
    /// between had no samples, and what the series moved over it is counted
    /// here, since when it moved is not known.
    pub gap: bool,
    /// Its value comes from less than its span: the range cut it, the
    /// store's clock has not reached its end, it begins before the oldest
    /// time its name keeps, or the step into its first sample began at one
    /// retention took. A chart shows such a bucket apart, or leaves an
    /// unfinished edge out: a `rate` there is a second of what is left of
    /// the bucket, and no speed the series ran at, since the step it counts
    /// may have begun before that.
    pub partial: bool,
}

/// A bucket's answer: one exact number, or where a timer's percentile lies.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// Exact and rounded once; an infinity past what a float holds.
    Exact(f64),
    /// A timer's percentile: the bucket of its spread that holds the rank.
    /// It gives no one number, since any would be read as the percentile.
    Between {
        /// Where that bucket begins, not in it: 0 below the first bound.
        lower: f64,
        /// Where it ends, in it: `f64::INFINITY` past the last bound.
        upper: f64,
        /// A point between them when the query asked for an `estimate`,
        /// interpolated as Prometheus' `histogram_quantile` does: past the
        /// last bound the last bound, and none when no bound lies below.
        estimate: Option<f64>,
    },
}

impl Value {
    /// The number, when the value is one.
    pub fn exact(self) -> Option<f64> {
        match self {
            Value::Exact(value) => Some(value),
            Value::Between { .. } => None,
        }
    }
}

/// Which series are one group.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Grouping {
    /// Every series of a name.
    Name,
    /// Those that share the labels named.
    By(Vec<String>),
    /// Those that share every label but the ones named.
    Without(Vec<String>),
}

/// What a query asks of an aggregate beside its `op`.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Asking<'a> {
    pub(super) every: Option<Duration>,
    /// Buckets of a calendar in the zone of its IANA name.
    pub(super) calendar: Option<(Calendar, &'a str)>,
    pub(super) by: Option<&'a [String]>,
    pub(super) without: Option<&'a [String]>,
    /// The name it asked for: a timer's is read as the series it writes.
    pub(super) name: Option<&'a str>,
    pub(super) estimate: bool,
}

/// An aggregate checked: its `op`, its buckets and its groups.
#[derive(Clone, Debug)]
pub(super) struct Shape {
    op: Op,
    rank: Option<Rank>,
    /// What a bucket spans; without it the range is one bucket.
    grid: Option<Grid>,
    grouping: Grouping,
    name: Option<String>,
    estimate: bool,
}

impl Shape {
    pub(super) fn of(op: Op, asking: Asking<'_>) -> Result<Shape> {
        let rank = match op {
            Op::Percentile(percent) => Some(Rank::of(percent)?),
            _ if asking.estimate => {
                return Err(Error::invalid(format!("an estimate of {op}: only a percentile has one")));
            }
            _ => None,
        };
        let grid = match (asking.every, asking.calendar) {
            (Some(_), Some(_)) => {
                return Err(Error::invalid("an aggregate's buckets are `every` or `calendar`, not both"));
            }
            (Some(span), None) if span.is_zero() || span.subsec_nanos() % 1_000_000 != 0 => {
                return Err(Error::invalid("every: a span of whole milliseconds, one at least"));
            }
            (Some(span), None) => Some(Grid::Every(crate::clock::millis(span))),
            (None, Some((unit, zone))) => Some(Grid::Calendar(Zoned::of(unit, zone)?)),
            (None, None) => None,
        };
        let grouping = match (asking.by, asking.without) {
            (Some(_), Some(_)) => {
                return Err(Error::invalid("an aggregate groups `by` labels or `without` them, not both"));
            }
            (Some(labels), None) => Grouping::By(grouped(labels)?),
            (None, Some(labels)) => Grouping::Without(grouped(labels)?),
            (None, None) => Grouping::Name,
        };
        let name = asking.name.map(str::to_owned);
        Ok(Shape { op, rank, grid, grouping, name, estimate: asking.estimate })
    }

    pub(super) fn op(&self) -> Op {
        self.op
    }

    pub(super) fn rank(&self) -> Option<Rank> {
        self.rank
    }

    pub(super) fn estimate(&self) -> bool {
        self.estimate
    }

    /// What a series is to the query: one of the name it asked, or a part of
    /// the timer it named.
    pub(super) fn part(&self, found: &Found) -> Part {
        Part::of(found.kept.name(), self.name.as_deref())
    }

    /// What one series' bucket keeps of its samples: the `op`, or what its
    /// part of a timer gives the timer's.
    pub(super) fn series_op(&self, found: &Found) -> Op {
        match self.part(found) {
            Part::Own if self.spreads(found) => Op::Increase,
            Part::Own => self.op,
            Part::Count | Part::Sum | Part::Buckets => Op::Increase,
            Part::Longest => Op::Max,
        }
    }

    /// Whether a series is a bucket of a timer's spread that a percentile
    /// ranks: its `_bucket`, or a histogram's of its own name.
    pub(super) fn spreads(&self, found: &Found) -> bool {
        matches!(self.op, Op::Percentile(_)) && (found.kind == Kind::Histogram || self.part(found) == Part::Buckets)
    }

    /// Whether an `op` counts steps between samples, the first of a bucket
    /// from the sample before it.
    pub(super) fn steps(op: Op) -> bool {
        matches!(op, Op::Increase | Op::Rate | Op::Delta)
    }

    /// Whether a block is answered from its summary for a series whose
    /// bucket keeps `op`: it keeps exact sums, every sample of it lies in the
    /// window and in one bucket, and the `op` takes no sample's rank.
    pub(super) fn holds(&self, op: Op, block: &Block, window: Window) -> bool {
        let (start, end) = (block.times.start, block.times.end);
        let one_bucket = self.grid.as_ref().is_none_or(|grid| grid.start(start) == grid.start(end));
        let ranks = matches!(op, Op::Percentile(_));
        !ranks && block.summary.exact.is_some() && start >= window.from && end < window.to && one_bucket
    }

    pub(super) fn check(&self, found: &Found) -> Result<()> {
        if self.part(found) != Part::Own || self.op.is_of(found.kind) {
            return Ok(());
        }
        let has = "a gauge has count, sum, avg, min, max, delta, first, last and percentiles; a counter count, \
                   increase, rate, first and last; a histogram's buckets those of a counter and percentiles";
        Err(Error::invalid(format!("{} of a {}: {has}", self.op, found.kind)).within(&found.kept))
    }

    /// The group a series falls in: its name, a timer's when it is one of the
    /// timer's parts, and the labels the query kept; a timer's `le` and the
    /// intervals of its grid are its buckets', which a percentile ranks
    /// together.
    ///
    /// ```text
    /// by route    http_requests_total{host="a",route="/x"}  →  http_requests_total{route="/x"}
    /// p95         http_request_ms_bucket{le="5",route="/x"} →  http_request_ms{}
    /// ```
    pub(super) fn group_of(&self, found: &Found) -> Result<(String, Labels)> {
        let part = self.part(found);
        let spreads = self.spreads(found);
        if spreads && matches!(&self.grouping, Grouping::By(names) if names.iter().any(|name| name == "le")) {
            return Err(Error::invalid("a percentile ranks a timer's buckets together: `by` keeps no `le`"));
        }
        let mut labels = found.kept.labels();
        if spreads {
            labels.remove("le");
            labels.remove(BIN);
        }
        labels.retain(|label, _| {
            label == OVERFLOW
                || match &self.grouping {
                    Grouping::Name => false,
                    Grouping::By(names) => names.contains(label),
                    Grouping::Without(names) => !names.contains(label),
                }
        });
        let name = match (&self.name, part) {
            (Some(timer), part) if part != Part::Own => timer.clone(),
            _ => found.kept.name().to_owned(),
        };
        Ok((name, labels))
    }

    /// Where a series stands among its group's: a timer's spread after its
    /// other parts, a member's buckets together, its `+Inf` first.
    pub(super) fn order_of(&self, found: &Found) -> (bool, Labels, bool) {
        if !self.spreads(found) {
            return (false, Labels::new(), false);
        }
        let mut member = found.kept.labels();
        let infinity = member.remove("le").is_some_and(|le| le == "+Inf");
        member.remove(BIN);
        (true, member, !infinity)
    }

    /// Where the bucket that holds `at` begins: a whole number of `every`
    /// since 1970, a calendar's local start, or where the range does when it
    /// is one bucket.
    pub(super) fn bucket_of(&self, at: i64, range_from: i64) -> i64 {
        self.grid.as_ref().map_or(range_from, |grid| grid.start(at))
    }

    /// What the range leaves of the bucket that begins at `at`.
    ///
    /// ```text
    /// every 1h, range 10:20 to 12:40    10:00 → 10:20 to 11:00    12:00 → 12:00 to 12:40
    /// ```
    pub(super) fn span(&self, at: i64, range_from: i64, to: i64) -> (i64, i64) {
        match &self.grid {
            Some(grid) => (at.max(range_from), grid.end(at).min(to)),
            None => (range_from, to),
        }
    }

    /// Whether a bucket's value comes from less than its span: the range cut
    /// it, the store's clock at `now` has not reached its end, or it begins
    /// before `cutoff`, the oldest time its name keeps.
    ///
    /// ```text
    /// every 1h, since 1h at 12:10    11:00 → cut at 11:10    12:00 → ten minutes of it so far
    /// ```
    pub(super) fn cuts(&self, at: i64, span: (i64, i64), cutoff: i64, now: i64) -> bool {
        let by_range = self.grid.as_ref().is_some_and(|grid| span.0 > at || span.1 < grid.end(at));
        by_range || span.1 > now.saturating_add(1) || span.0 < cutoff
    }

    /// Whether a step from a sample at `before` into the bucket at `at`
    /// began before the bucket before it, so that a whole bucket between
    /// them had no samples. The yardstick is the query's buckets, the range
    /// when it is one bucket, never a guess at how often a series writes.
    pub(super) fn gap(&self, before: i64, at: i64, span: (i64, i64)) -> bool {
        match &self.grid {
            Some(grid) => before < grid.start(at.saturating_sub(1)),
            None => before < at.saturating_sub(span.1.saturating_sub(span.0)),
        }
    }
}

/// What a bucket spans, in milliseconds since 1970.
#[derive(Clone, Debug)]
enum Grid {
    /// A width counted from 1970 in UTC.
    Every(i64),
    /// A span of a calendar in a time zone, as long as that span is there.
    Calendar(Zoned),
}

impl Grid {
    /// Where the bucket that holds `at` begins.
    fn start(&self, at: i64) -> i64 {
        match self {
            Grid::Every(every) => at.div_euclid(*every) * every,
            Grid::Calendar(zoned) => zoned.start_millis(at),
        }
    }

    /// Where the bucket that begins at `start` ends.
    fn end(&self, start: i64) -> i64 {
        match self {
            Grid::Every(every) => start.saturating_add(*every),
            Grid::Calendar(zoned) => zoned.end_millis(start),
        }
    }
}

/// The labels a grouping names, each one a program could have given a series.
fn grouped(labels: &[String]) -> Result<Vec<String>> {
    if labels.len() > MOST_LABELS {
        return Err(Error::invalid(format!("a grouping of more than {MOST_LABELS} labels")));
    }
    for label in labels {
        if label.is_empty() || label.len() > MOST_NAME_BYTES || super::labels::is_own(label) {
            return Err(Error::invalid(format!(
                "group by label {label:?}: a name of 1 to {MOST_NAME_BYTES} bytes that does not begin with __"
            )));
        }
    }
    Ok(labels.to_vec())
}

#[cfg(test)]
#[path = "aggregate_tests.rs"]
mod tests;
