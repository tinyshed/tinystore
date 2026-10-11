//! `aggregate`: a query's range cut into buckets of `every`, each one exact
//! value of an `op`, a group of series a bucket.
//!
//! ```no_run
//! # use std::time::Duration;
//! # fn f(stats: tinystore::metrics::Metrics) -> tinystore::Result<()> {
//! use tinystore::metrics::Op;
//! let traffic = stats
//!     .query()
//!     .name("http_requests_total")
//!     .since(Duration::from_hours(24))
//!     .every(Duration::from_hours(1))
//!     .by(&["route"])
//!     .aggregate(Op::Increase)?;
//! for route in traffic {
//!     println!("{}: {:?}", route.labels["route"], route.buckets.iter().map(|hour| hour.value).collect::<Vec<_>>());
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Buckets are spans of `every` counted from 1970 in UTC, and the range cuts
//! the first and the last. A series' samples reach its buckets in the order
//! of their times, a block inside one bucket whole, from its summary, and a
//! step between two samples is counted in the bucket it ends in:
//!
//! ```text
//! samples   09:59:45   10:00:00 … 10:59:45   11:00:00 …
//! buckets   ────────── 10:00 ─────────────── 11:00 ──────
//!           carried into 10:00's first step  the step from 10:59:45 counts at 11:00
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, SystemTime};

use super::bucket::{Joined, Open};
use super::budget::Budget;
use super::codec::Point;
use super::directory::Block;
use super::engine::Engine;
use super::labels::{Kept, MOST_LABELS, MOST_NAME_BYTES};
use super::matching::Found;
use super::query::Asked;
use super::read::PATIENCE;
use super::series::{Kind, Labels};
use super::snapshot::{Taken, Want, Window, decode_block};
use super::timestamp::rfc3339;
use crate::compress::frame::Zstd;
use crate::{Error, Reservation, Result};

/// The label of the series an instrument counts into past its sets of
/// labels, which is a group of its own whatever a query groups by.
const OVERFLOW: &str = "__overflow";
/// What a bucket of a group holds of the store's memory while it is counted.
const BUCKET_HELD: u64 = 512;

/// What a bucket's value is of its samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Op {
    /// How many samples.
    Count,
    /// A gauge's exact sum.
    Sum,
    /// A gauge's sum over its count.
    Avg,
    /// A gauge's least sample.
    Min,
    /// A gauge's greatest sample.
    Max,
    /// How much a counter grew, a reset counted as its new value.
    Increase,
    /// A counter's increase a second of the bucket.
    Rate,
    /// How much a gauge moved: its last sample less the one before its
    /// first.
    Delta,
    /// The first sample, bit for bit; a group adds its series'.
    First,
    /// The last sample, bit for bit; a group adds its series'.
    Last,
}

impl Op {
    /// The `op` as the wire and every SDK say it.
    pub fn as_str(self) -> &'static str {
        match self {
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
        }
    }

    /// Whether a series of `kind` has it; a histogram's `_bucket` series is
    /// a counter.
    fn is_of(self, kind: Kind) -> bool {
        match self {
            Op::Count | Op::First | Op::Last => true,
            Op::Sum | Op::Avg | Op::Min | Op::Max | Op::Delta => kind == Kind::Gauge,
            Op::Increase | Op::Rate => kind != Kind::Gauge,
        }
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A group of an aggregate: the series of one name that share the labels the
/// query kept, and their buckets in the order of their times.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Aggregate {
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
    /// Its answer, exact and rounded once; an infinity past what a float
    /// holds.
    pub value: f64,
    /// How many samples it comes from.
    pub samples: u64,
    /// A counter's resets in it.
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

/// An aggregate checked: its `op`, its buckets and its groups.
#[derive(Clone, Debug)]
pub(super) struct Shape {
    op: Op,
    /// A bucket's width in milliseconds; without one the range is one bucket.
    every: Option<i64>,
    grouping: Grouping,
}

impl Shape {
    pub(super) fn of(
        op: Op,
        every: Option<Duration>,
        by: Option<&[String]>,
        without: Option<&[String]>,
    ) -> Result<Shape> {
        let every = match every {
            None => None,
            Some(span) if span.is_zero() || span.subsec_nanos() % 1_000_000 != 0 => {
                return Err(Error::invalid("every: a span of whole milliseconds, one at least"));
            }
            Some(span) => Some(i64::try_from(span.as_millis()).unwrap_or(i64::MAX)),
        };
        let grouping = match (by, without) {
            (Some(_), Some(_)) => {
                return Err(Error::invalid("an aggregate groups `by` labels or `without` them, not both"));
            }
            (Some(labels), None) => Grouping::By(grouped(labels)?),
            (None, Some(labels)) => Grouping::Without(grouped(labels)?),
            (None, None) => Grouping::Name,
        };
        Ok(Shape { op, every, grouping })
    }

    /// Whether the `op` counts steps between samples, the first of a bucket
    /// from the sample before it.
    pub(super) fn steps(&self) -> bool {
        matches!(self.op, Op::Increase | Op::Rate | Op::Delta)
    }

    /// Whether a block is answered from its summary: it keeps exact sums,
    /// and every sample of it lies in the window and in one bucket.
    pub(super) fn holds(&self, block: &Block, window: Window) -> bool {
        let (start, end) = (block.times.start, block.times.end);
        let one_bucket = self.every.is_none_or(|every| start.div_euclid(every) == end.div_euclid(every));
        block.summary.exact.is_some() && start >= window.from && end < window.to && one_bucket
    }

    pub(super) fn check(&self, found: &Found) -> Result<()> {
        if self.op.is_of(found.kind) {
            return Ok(());
        }
        let has = "a gauge has count, sum, avg, min, max, delta, first and last; a counter count, increase, rate, \
                   first and last";
        Err(Error::invalid(format!("{} of a {}: {has}", self.op, found.kind)).within(&found.kept))
    }

    /// The group a series falls in: its name, and the labels the query kept.
    ///
    /// ```text
    /// by route    http_requests_total{host="a",route="/x"}  →  http_requests_total{route="/x"}
    /// ```
    fn group_of(&self, kept: &Kept) -> (String, Labels) {
        let mut labels = kept.labels();
        labels.retain(|label, _| {
            label == OVERFLOW
                || match &self.grouping {
                    Grouping::Name => false,
                    Grouping::By(names) => names.contains(label),
                    Grouping::Without(names) => !names.contains(label),
                }
        });
        (kept.name().to_owned(), labels)
    }

    /// Where the bucket that holds `at` begins: a whole number of `every`
    /// since 1970, or where the range does when it is one bucket.
    fn bucket_of(&self, at: i64, range_from: i64) -> i64 {
        self.every.map_or(range_from, |every| at.div_euclid(every) * every)
    }

    /// What the range leaves of the bucket that begins at `at`.
    ///
    /// ```text
    /// every 1h, range 10:20 to 12:40    10:00 → 10:20 to 11:00    12:00 → 12:00 to 12:40
    /// ```
    fn span(&self, at: i64, range_from: i64, to: i64) -> (i64, i64) {
        match self.every {
            Some(every) => (at.max(range_from), at.saturating_add(every).min(to)),
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
    fn cuts(&self, at: i64, span: (i64, i64), cutoff: i64, now: i64) -> bool {
        let by_range = self.every.is_some_and(|every| span.0 > at || span.1 < at.saturating_add(every));
        by_range || span.1 > now.saturating_add(1) || span.0 < cutoff
    }

    /// Whether a step from a sample at `before` into the bucket at `at`
    /// began before the bucket before it, so that a whole bucket between
    /// them had no samples. The yardstick is the query's `every`, the range
    /// when it is one bucket, never a guess at how often a series writes.
    fn gap(&self, before: i64, at: i64, span: (i64, i64)) -> bool {
        let width = self.every.unwrap_or(span.1.saturating_sub(span.0));
        before < at.saturating_sub(width)
    }
}

/// The labels a grouping names, each one a program could have given a series.
fn grouped(labels: &[String]) -> Result<Vec<String>> {
    if labels.len() > MOST_LABELS {
        return Err(Error::invalid(format!("a grouping of more than {MOST_LABELS} labels")));
    }
    for label in labels {
        if label.is_empty() || label.len() > MOST_NAME_BYTES || label.starts_with("__") {
            return Err(Error::invalid(format!(
                "group by label {label:?}: a name of 1 to {MOST_NAME_BYTES} bytes that does not begin with __"
            )));
        }
    }
    Ok(labels.to_vec())
}

impl Engine {
    pub(super) fn aggregate(&self, asked: &Asked, shape: &Shape) -> Result<Vec<Aggregate>> {
        let mut budget = Budget::default();
        let taken = self.take(asked, Want::Buckets(shape), &mut budget)?;
        let mut held = self.memory.reserve(budget.bytes as u64, "metrics: an aggregate", PATIENCE)?;
        let mut zstd = self.codecs.take();
        let mut counting =
            Counting { shape, asked, zstd: &mut zstd, budget: &mut budget, held: &mut held, points: Vec::new() };
        let answer = counting.groups(taken);
        self.codecs.give_back(zstd);
        answer
    }
}

/// One aggregate being answered from what a snapshot took.
struct Counting<'a> {
    shape: &'a Shape,
    asked: &'a Asked,
    zstd: &'a mut Zstd,
    budget: &'a mut Budget,
    held: &'a mut Reservation,
    /// The samples of the block or the head being counted.
    points: Vec<Point>,
}

impl Counting<'_> {
    /// Every group's buckets, the groups in the order of their names and
    /// labels; one without a bucket is left out.
    fn groups(&mut self, taken: Vec<Taken>) -> Result<Vec<Aggregate>> {
        let mut members: BTreeMap<(String, Labels), Vec<Taken>> = BTreeMap::new();
        for series in taken {
            self.shape.check(&series.found)?;
            members.entry(self.shape.group_of(&series.found.kept)).or_default().push(series);
        }
        let mut answer = Vec::with_capacity(members.len());
        for ((name, labels), members) in members {
            let buckets = self.group(&members)?;
            if !buckets.is_empty() {
                answer.push(Aggregate { name, labels, buckets });
            }
        }
        Ok(answer)
    }

    /// One group's buckets: each series' counted exactly and joined, a group
    /// at a time, so that the store's memory holds one group's sums.
    fn group(&mut self, members: &[Taken]) -> Result<Vec<Bucket>> {
        let mut joined = BTreeMap::new();
        for series in members {
            self.series(series, &mut joined).map_err(|error| error.within(&series.found.kept))?;
        }
        let buckets = joined.iter().map(|(at, bucket)| bucket.answer(*at, self.shape.op)).collect();
        self.held.shrink(self.budget.bytes as u64);
        Ok(buckets)
    }

    /// Counts one series into its group's buckets: its blocks, whole from
    /// their summaries or decoded, then its head.
    fn series(&mut self, series: &Taken, joined: &mut BTreeMap<i64, Joined>) -> Result<()> {
        let mut fold = Fold::new(self.shape, series, self.asked);
        for part in &series.blocks {
            if part.whole {
                fold.block(&part.block)?;
            } else {
                self.points.clear();
                decode_block(self.zstd, &part.block, &mut self.points)?;
                self.points.iter().try_for_each(|point| fold.sample(*point))?;
            }
            self.join(&mut fold, joined)?;
        }
        self.points.clear();
        series.decode_head(self.zstd, &mut self.points)?;
        self.points.iter().try_for_each(|point| fold.sample(*point))?;
        fold.close();
        self.join(&mut fold, joined)
    }

    /// Adds a series' closed buckets to its group's; a bucket new to the
    /// group counts against the answer's bound.
    fn join(&mut self, fold: &mut Fold<'_>, joined: &mut BTreeMap<i64, Joined>) -> Result<()> {
        for bucket in fold.closed.drain(..) {
            if !joined.contains_key(&bucket.at) {
                self.budget.take_answer(1)?;
                self.held.grow(BUCKET_HELD, "metrics: an aggregate's buckets")?;
            }
            joined.entry(bucket.at).or_insert_with(|| Joined::over(&bucket)).join(&bucket, self.shape.op);
        }
        Ok(())
    }
}

/// One series' samples counted into its buckets, in the order of their
/// times.
struct Fold<'a> {
    shape: &'a Shape,
    kind: Kind,
    window: Window,
    /// Where the query's range begins: where the name's `keep` does when the
    /// query says no start.
    range_from: i64,
    /// Retention has taken samples of the series, or hides some.
    trimmed: bool,
    /// The store's clock, as the query read it.
    now: i64,
    open: Option<Open>,
    /// Its last sample so far, which the next bucket's first step begins at.
    carried: Option<Point>,
    /// Whether a sample of the range has come: until one does, what is
    /// carried came from before it and was not checked.
    entered: bool,
    newest: Option<i64>,
    closed: Vec<Open>,
}

impl<'a> Fold<'a> {
    fn new(shape: &'a Shape, series: &Taken, asked: &Asked) -> Fold<'a> {
        let window = series.window;
        Fold {
            shape,
            kind: series.found.kind,
            window,
            range_from: if asked.from == i64::MIN { window.cutoff } else { asked.from },
            trimmed: series.trimmed,
            now: asked.now,
            open: None,
            carried: None,
            entered: false,
            newest: None,
            closed: Vec::new(),
        }
    }

    fn sample(&mut self, point: Point) -> Result<()> {
        if point.at >= self.window.to {
            return Ok(());
        }
        if point.at < self.window.cutoff {
            // one its name keeps no longer, not out of the file yet
            self.trimmed = true;
            return Ok(());
        }
        self.follows(point.at)?;
        if point.at < self.window.from {
            // before the range: the first step of its first bucket begins at the newest such
            self.carried = Some(point);
            return Ok(());
        }
        self.reach()?;
        self.check(point)?;
        let op = self.shape.op;
        self.enter(point.at).add(point.value, op);
        self.carried = Some(point);
        Ok(())
    }

    /// A block answered from its directory: one before the range, of which
    /// only the last sample counts, or one inside a bucket.
    fn block(&mut self, block: &Block) -> Result<()> {
        self.follows(block.times.start)?;
        self.newest = Some(block.times.end);
        let last = Point { at: block.times.end, value: block.summary.last };
        if last.at < self.window.from {
            self.carried = Some(last);
            return Ok(());
        }
        self.reach()?;
        let op = self.shape.op;
        self.enter(block.times.start).add_block(block, op)?;
        self.carried = Some(last);
        Ok(())
    }

    fn follows(&mut self, at: i64) -> Result<()> {
        if self.newest.is_some_and(|newest| at <= newest) {
            return Err(Error::corrupt("samples out of the order of their times"));
        }
        self.newest = Some(at);
        Ok(())
    }

    /// Refuses a sample no aggregate takes: none takes a value for a missing
    /// one, nor a counter below zero for a count.
    fn check(&self, point: Point) -> Result<()> {
        let refused = if !point.value.is_finite() {
            "an aggregate over a NaN or an infinity"
        } else if self.kind != Kind::Gauge && point.value < 0.0 {
            "an aggregate over a counter below zero"
        } else {
            return Ok(());
        };
        Err(Error::invalid(refused).within(format!("sample at {}", rfc3339(point.at))))
    }

    /// Reaches the range: of the samples before it only the newest counts,
    /// as where a first step begins, so it alone is checked.
    fn reach(&mut self) -> Result<()> {
        let first = !std::mem::replace(&mut self.entered, true);
        match self.carried {
            Some(before) if first && self.shape.steps() => self.check(before),
            _ => Ok(()),
        }
    }

    /// The open bucket once it is the one that holds `at`, the one before it
    /// closed. A bucket's first step begins at the sample the series carried
    /// into it.
    fn enter(&mut self, at: i64) -> &mut Open {
        let key = self.shape.bucket_of(at, self.range_from);
        if self.open.as_ref().is_some_and(|open| open.at != key) {
            self.close();
        }
        let (shape, window, range_from) = (self.shape, self.window, self.range_from);
        let (trimmed, before, now) = (self.trimmed, self.carried, self.now);
        self.open.get_or_insert_with(|| {
            let span = shape.span(key, range_from, window.to);
            let mut open = Open::new(key, span);
            open.partial = shape.cuts(key, span, window.cutoff, now);
            match before {
                Some(before) if shape.steps() => {
                    open.step_from(before.value);
                    open.gap = shape.gap(before.at, key, span);
                }
                // the step into a series' oldest sample began at one retention took
                None if shape.steps() => open.partial |= trimmed,
                _ => {}
            }
            open
        })
    }

    fn close(&mut self) {
        self.closed.extend(self.open.take());
    }
}

#[cfg(test)]
#[path = "aggregate_tests.rs"]
mod tests;
