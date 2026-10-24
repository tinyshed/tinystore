//! An aggregate counted from what one snapshot took: each series' samples
//! folded into its buckets exactly, in the order of their times, then joined
//! into its group's, a group at a time, so that the store's memory holds one
//! group's sums.
//!
//! A timer's percentile joins a member at a time, its `+Inf` first: a bound
//! counts toward the group only where it counted as many samples as the
//! member's `+Inf`, which is where the member's set held it.

use std::collections::BTreeMap;

use super::aggregate::{Aggregate, Bucket, Op, Shape};
use super::bucket::{Joined, Open};
use super::buckets::bound_of;
use super::budget::Budget;
use super::codec::Point;
use super::directory::Block;
use super::engine::Engine;
use super::grid::Bin;
use super::labels::BIN;
use super::query::Asked;
use super::read::PATIENCE;
use super::series::{Kind, Labels};
use super::snapshot::{Taken, Want, Window, decode_block};
use super::timer::Part;
use super::timestamp::rfc3339;
use crate::compress::frame::Zstd;
use crate::{Error, Reservation, Result};

/// What a bucket of a group holds of the store's memory while it is counted.
const BUCKET_HELD: u64 = 512;

impl Engine {
    pub(super) fn aggregate(&self, asked: &Asked, shape: &Shape) -> Result<Vec<Aggregate>> {
        let mut budget = Budget::default();
        let taken = self.take(asked, Want::Buckets(shape), &mut budget)?;
        let mut held = self.memory.reserve_for_read(budget.bytes as u64, "metrics: an aggregate", PATIENCE)?;
        let mut zstd = self.codecs.take();
        let mut counting = Counting {
            engine: self,
            shape,
            asked,
            zstd: &mut zstd,
            budget: &mut budget,
            held: &mut held,
            points: Vec::new(),
        };
        let answer = counting.groups(taken);
        self.codecs.give_back(zstd);
        answer
    }
}

/// One aggregate being answered from what a snapshot took.
struct Counting<'a> {
    engine: &'a Engine,
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
        let mut groups: BTreeMap<(String, Labels), Vec<Taken>> = BTreeMap::new();
        for series in taken {
            self.shape.check(&series.found)?;
            let group = self.shape.group_of(&series.found).map_err(|error| error.within(&series.found.kept))?;
            groups.entry(group).or_default().push(series);
        }
        let mut answer = Vec::with_capacity(groups.len());
        for ((name, labels), members) in groups {
            let buckets = self.group(members)?;
            if !buckets.is_empty() {
                answer.push(Aggregate { name, labels, buckets });
            }
        }
        Ok(answer)
    }

    /// One group's buckets: each series' counted exactly and joined, a
    /// timer's spread a member at a time, its `+Inf` first.
    fn group(&mut self, mut members: Vec<Taken>) -> Result<Vec<Bucket>> {
        members.sort_by_cached_key(|series| self.shape.order_of(&series.found));
        let mut joining = Joining::new(self.shape);
        for series in &members {
            crate::cancel::check(|| "an aggregate".to_owned())?;
            let counted = joining.begin(series).and_then(|()| self.series(series, &mut joining));
            counted.map_err(|error| self.engine.met(&series.found, error))?;
        }
        let buckets = joining.answer()?;
        self.held.shrink(self.budget.bytes as u64);
        Ok(buckets)
    }

    /// Counts one series into its group's buckets: its blocks, whole from
    /// their summaries or decoded, then its head.
    fn series(&mut self, series: &Taken, joining: &mut Joining<'_>) -> Result<()> {
        let mut fold = Fold::new(self.shape, series, self.asked);
        for part in &series.blocks {
            if part.whole {
                fold.block(&part.block)?;
            } else {
                self.points.clear();
                decode_block(self.zstd, &part.block, &mut self.points)?;
                self.points.iter().try_for_each(|point| fold.sample(*point))?;
            }
            self.join(&mut fold, joining)?;
        }
        self.points.clear();
        series.decode_head(self.zstd, &mut self.points)?;
        self.points.iter().try_for_each(|point| fold.sample(*point))?;
        fold.close();
        self.join(&mut fold, joining)
    }

    /// Adds a series' closed buckets to its group's: a bucket new to the
    /// group counts against the answer's bound, and what each holds against
    /// the store's memory.
    fn join(&mut self, fold: &mut Fold<'_>, joining: &mut Joining<'_>) -> Result<()> {
        for bucket in fold.closed.drain(..) {
            let mut more = 0;
            if !joining.holds(bucket.at) {
                self.budget.take_answer(1)?;
                more += BUCKET_HELD;
            }
            more += joining.join(bucket)? as u64;
            if more > 0 {
                self.held.grow(more, "metrics: an aggregate's buckets")?;
            }
        }
        Ok(())
    }
}

/// A group's buckets being joined, and what the series being folded gives
/// them.
struct Joining<'s> {
    shape: &'s Shape,
    joined: BTreeMap<i64, Joined>,
    part: Part,
    /// The bound or the interval of a timer's spread the series counts.
    counts: Option<Counts>,
    /// The member of a timer's spread being joined, by its labels.
    member: Option<Labels>,
    /// How many samples the member's `+Inf` has in each bucket, by where the
    /// bucket begins: none where it counted no value, and the member takes
    /// no part.
    infinity: BTreeMap<i64, Option<u64>>,
}

/// What a series of a timer's spread counts.
#[derive(Clone, Copy, Debug)]
enum Counts {
    /// The values at most a bound, `+Inf` every value.
    AtMost(f64),
    /// The values of one interval of an exponential grid.
    Within(Bin),
}

impl<'s> Joining<'s> {
    fn new(shape: &'s Shape) -> Joining<'s> {
        Joining {
            shape,
            joined: BTreeMap::new(),
            part: Part::Own,
            counts: None,
            member: None,
            infinity: BTreeMap::new(),
        }
    }

    /// Takes the next series: what it is to the query, and for a timer's
    /// spread its member and what it counts.
    fn begin(&mut self, series: &Taken) -> Result<()> {
        let found = &series.found;
        self.part = self.shape.part(found);
        self.counts = None;
        if !self.shape.spreads(found) {
            return Ok(());
        }
        let mut member = found.kept.labels();
        let (le, bin) = (member.remove("le"), member.remove(BIN));
        if self.member.as_ref() != Some(&member) {
            self.member = Some(member);
            self.infinity.clear();
        }
        self.counts = Some(match (le, bin) {
            (Some(le), None) => {
                let bound = bound_of(&le).ok_or_else(|| Error::invalid(format!("a bucket's le {le:?}, no bound")))?;
                Counts::AtMost(bound)
            }
            (None, Some(bin)) => Counts::Within(
                Bin::parse(&bin).ok_or_else(|| Error::corrupt(format!("an interval {bin:?} of no grid")))?,
            ),
            _ => return Err(Error::invalid("a histogram's bucket without its bound, `le`")),
        });
        Ok(())
    }

    fn holds(&self, at: i64) -> bool {
        self.joined.contains_key(&at)
    }

    /// Joins a series' closed bucket into its group's, and gives the bytes
    /// that holds more.
    fn join(&mut self, bucket: Open) -> Result<usize> {
        let at = bucket.at;
        let part = self.part;
        let joined = self.joined.entry(at).or_insert_with(|| Joined::over(&bucket));
        Ok(match self.counts {
            None => joined.join(bucket, self.shape.op(), part),
            Some(Counts::AtMost(bound)) if bound == f64::INFINITY => {
                self.infinity.insert(at, bucket.counted().then_some(bucket.samples));
                joined.join_infinity(&bucket, part)
            }
            Some(Counts::AtMost(bound)) => match self.infinity.get(&at) {
                None => {
                    let at = rfc3339(at);
                    return Err(Error::invalid(format!(
                        "a histogram's buckets without `+Inf` in the bucket at {at}: its values are not all counted"
                    )));
                }
                Some(None) => joined.join_bound(&bucket, bound, false),
                Some(Some(samples)) => joined.join_bound(&bucket, bound, *samples == bucket.samples),
            },
            Some(Counts::Within(bin)) => joined.join_bin(&bucket, bin, part),
        })
    }

    /// The group's buckets in the order of their times, those an `op` has
    /// nothing to take from left out.
    fn answer(self) -> Result<Vec<Bucket>> {
        let mut buckets = Vec::with_capacity(self.joined.len());
        for (at, mut joined) in self.joined {
            buckets.extend(joined.answer(at, self.shape)?);
        }
        Ok(buckets)
    }
}

/// One series' samples counted into its buckets, in the order of their
/// times.
struct Fold<'a> {
    shape: &'a Shape,
    /// What its buckets keep of its samples.
    op: Op,
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
            op: shape.series_op(&series.found),
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
        let op = self.op;
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
        let op = self.op;
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
            Some(before) if first && Shape::steps(self.op) => self.check(before),
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
        let (shape, window, range_from, steps) = (self.shape, self.window, self.range_from, Shape::steps(self.op));
        let (trimmed, before, now) = (self.trimmed, self.carried, self.now);
        self.open.get_or_insert_with(|| {
            let span = shape.span(key, range_from, window.to);
            let mut open = Open::new(key, span);
            open.partial = shape.cuts(key, span, window.cutoff, now);
            match before {
                Some(before) if steps => {
                    open.step_from(before.value);
                    open.gap = shape.gap(before.at, key, span);
                }
                // the step into a series' oldest sample began at one retention took
                None if steps => open.partial |= trimmed,
                _ => {}
            }
            open
        })
    }

    fn close(&mut self) {
        self.closed.extend(self.open.take());
    }
}
