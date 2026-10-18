//! A bucket of an aggregate being counted: one series' samples in it first,
//! exactly, then its group's, which each series' bucket joins, rounded once
//! at the end.
//!
//! | `op`               | A series' bucket keeps                        | A group                        |
//! |--------------------|-----------------------------------------------|--------------------------------|
//! | `count`            | how many samples                              | adds them                      |
//! | `sum`, `avg`       | their exact sum                               | adds sums and counts           |
//! | `min`, `max`       | the least and the greatest, `-0` before `+0`  | takes the least, the greatest  |
//! | `increase`, `rate` | the exact sum of its steps, a reset's as its  | adds them                      |
//! |                    | new value, and its resets                     |                                |
//! | `delta`            | its last sample less the one before its first | adds them                      |
//! | `first`, `last`    | the sample, bit for bit                       | adds them, alone gives the one |
//! | a percentile       | a gauge's samples; a timer's bucket's count   | ranks them all; joins a spread |
//!
//! A step is counted in the bucket it ends in: a bucket's first step begins
//! at the sample before the bucket, which the series carries into it.
//!
//! ```text
//! 100 → 110 │ 5 → 20        increases 10 │ 5 + 15, a reset between them
//! ```
//!
//! A timer's name joins its parts: its count's increase is how many
//! durations, which every `op` of it gives as `samples`, its sum's the sum of
//! them, its longest's greatest the longest, and its buckets' its spread.

use std::mem::size_of;

use super::aggregate::{Bucket, Op, Shape, Value};
use super::directory::Block;
use super::exact::{BLOCK_BITS, Exact};
use super::grid::Bin;
use super::rank::Tally;
use super::spread::Spread;
use super::summary::{signed_greatest, signed_least};
use super::timer::Part;
use crate::clock::from_unix_millis;
use crate::compress::varint::Reader;
use crate::{Error, Result};

/// One series' samples in one bucket.
#[derive(Clone, Debug)]
pub(super) struct Open {
    /// Where the bucket begins, which tells it from another.
    pub(super) at: i64,
    /// What the query's range leaves of it, `[start, end)`.
    start: i64,
    end: i64,
    pub(super) samples: u64,
    resets: u64,
    least: f64,
    greatest: f64,
    first: f64,
    last: f64,
    /// The exact sum of its samples, or of its steps.
    exact: Exact,
    /// A percentile's samples, which it ranks.
    values: Vec<f64>,
    /// The sample before its first, which its first step begins at.
    before: Option<f64>,
    pub(super) gap: bool,
    pub(super) partial: bool,
}

impl Open {
    pub(super) fn new(at: i64, (start, end): (i64, i64)) -> Open {
        Open {
            at,
            start,
            end,
            samples: 0,
            resets: 0,
            least: 0.0,
            greatest: 0.0,
            first: 0.0,
            last: 0.0,
            exact: Exact::ZERO,
            values: Vec::new(),
            before: None,
            gap: false,
            partial: false,
        }
    }

    /// Begins its first step at the sample before the bucket.
    pub(super) fn step_from(&mut self, before: f64) {
        self.before = Some(before);
    }

    /// Whether a counter's steps in it counted anything.
    pub(super) fn counted(&self) -> bool {
        !self.exact.is_zero()
    }

    /// One sample more, a finite one.
    pub(super) fn add(&mut self, value: f64, op: Op) {
        self.widen(value, value, value);
        match op {
            Op::Sum | Op::Avg => {
                self.exact.add_float(value);
            }
            Op::Increase | Op::Rate => self.step(value),
            Op::Percentile(_) => self.values.push(value),
            _ => {}
        }
        self.last = value;
        self.samples += 1;
    }

    /// A whole block more, from its summary: its samples are all finite,
    /// since it keeps exact sums.
    pub(super) fn add_block(&mut self, block: &Block, op: Op) -> Result<()> {
        let summary = &block.summary;
        let sums = summary.exact.as_ref().ok_or_else(|| Error::internal("a block without exact sums read whole"))?;
        self.widen(block.first, summary.min, summary.max);
        match op {
            Op::Sum | Op::Avg => self.exact.add(&read(&sums.sum)?),
            Op::Increase | Op::Rate => {
                self.step(block.first);
                let increase = read(&sums.increase)?;
                if increase.is_negative() {
                    return Err(Error::corrupt("a block's increase below zero"));
                }
                self.exact.add(&increase);
                self.resets += u64::from(summary.resets);
            }
            _ => {}
        }
        self.last = summary.last;
        self.samples += block.times.count as u64;
        Ok(())
    }

    /// Takes in samples whose first is `first` and which lie between `least`
    /// and `greatest`.
    fn widen(&mut self, first: f64, least: f64, greatest: f64) {
        if self.samples == 0 {
            (self.first, self.least, self.greatest) = (first, least, greatest);
        } else {
            self.least = signed_least(self.least, least);
            self.greatest = signed_greatest(self.greatest, greatest);
        }
    }

    /// Counts the step that ends at `value`: what the counter grew by, or
    /// its new value where it fell, a reset.
    fn step(&mut self, value: f64) {
        let Some(previous) = (if self.samples == 0 { self.before } else { Some(self.last) }) else { return };
        self.exact.add_float(value);
        if value < previous {
            self.resets += 1;
        } else {
            self.exact.sub_float(previous);
        }
    }

    /// What a group adds of it, exactly.
    fn total(&self, op: Op) -> Exact {
        let mut total = Exact::ZERO;
        match op {
            Op::Delta => {
                total.add_float(self.last);
                total.sub_float(self.before.unwrap_or(self.first));
            }
            Op::First => {
                total.add_float(self.first);
            }
            Op::Last => {
                total.add_float(self.last);
            }
            _ => total = self.exact,
        }
        total
    }
}

/// A block's exact sum, as its directory spells it.
fn read(bytes: &[u8]) -> Result<Exact> {
    let mut reader = Reader::new(bytes);
    let exact = Exact::read(&mut reader, BLOCK_BITS);
    reader.finish().map_err(|error| error.within("a block's exact sum"))?;
    Ok(exact)
}

/// A group's bucket: each of its series' joined, exactly.
#[derive(Debug)]
pub(super) struct Joined {
    start: i64,
    end: i64,
    samples: u64,
    resets: u64,
    least: f64,
    greatest: f64,
    /// The first or the last sample of its one series, bit for bit.
    alone: f64,
    exact: Exact,
    series: u32,
    gap: bool,
    partial: bool,
    /// A gauge's percentile: every sample of the group in the bucket.
    values: Vec<f64>,
    /// A timer's name joined its parts here: its count's increase is in
    /// `exact`, and how many durations in `samples`.
    timer: bool,
    /// The sum of a timer's durations.
    sum: Option<Box<Exact>>,
    longest: Option<f64>,
    spread: Option<Box<Spread>>,
}

impl Joined {
    /// A group's bucket that holds nothing yet, over the span of `bucket`.
    pub(super) fn over(bucket: &Open) -> Joined {
        Joined {
            start: bucket.start,
            end: bucket.end,
            samples: 0,
            resets: 0,
            least: bucket.least,
            greatest: bucket.greatest,
            alone: 0.0,
            exact: Exact::ZERO,
            series: 0,
            gap: false,
            partial: false,
            values: Vec::new(),
            timer: false,
            sum: None,
            longest: None,
            spread: None,
        }
    }

    /// Adds a series' closed bucket as its part of the query takes it, and
    /// gives the bytes it holds more.
    pub(super) fn join(&mut self, mut bucket: Open, op: Op, part: Part) -> usize {
        self.partial |= bucket.partial;
        self.timer |= part != Part::Own;
        match part {
            Part::Own => {
                self.least = signed_least(self.least, bucket.least);
                self.greatest = signed_greatest(self.greatest, bucket.greatest);
                self.alone = if op == Op::First { bucket.first } else { bucket.last };
                self.exact.add(&bucket.total(op));
                self.samples += bucket.samples;
                self.resets += bucket.resets;
                self.series += 1;
                self.gap |= bucket.gap;
                let more = bucket.values.len() * size_of::<f64>();
                self.values.append(&mut bucket.values);
                more
            }
            Part::Count => {
                self.samples += count_of(&bucket.exact);
                self.exact.add(&bucket.exact);
                self.resets += bucket.resets;
                self.series += 1;
                self.gap |= bucket.gap;
                0
            }
            Part::Sum => {
                let more = if self.sum.is_some() { 0 } else { size_of::<Exact>() };
                self.sum.get_or_insert_default().add(&bucket.exact);
                more
            }
            Part::Longest => {
                self.longest =
                    Some(self.longest.map_or(bucket.greatest, |longest| signed_greatest(longest, bucket.greatest)));
                0
            }
            // a spread joins bound by bound, through the calls below
            Part::Buckets => 0,
        }
    }

    /// A member's `+Inf`: the count of every value of it, and, where the
    /// query's name is the histogram's own, its resets and gap. A member
    /// that counted nothing here takes no part in the spread, so that a
    /// process idle in the bucket narrows no other's bounds.
    pub(super) fn join_infinity(&mut self, bucket: &Open, part: Part) -> usize {
        self.partial |= bucket.partial;
        if part == Part::Own {
            self.resets += bucket.resets;
            self.gap |= bucket.gap;
        }
        self.timer |= part != Part::Own;
        if !bucket.counted() {
            return 0;
        }
        let (spread, more) = self.spread();
        more + spread.add_infinity(&bucket.exact)
    }

    /// A bound of a member's set, which it holds here when it counted as
    /// many samples as its `+Inf`.
    pub(super) fn join_bound(&mut self, bucket: &Open, bound: f64, held: bool) -> usize {
        self.partial |= bucket.partial;
        if !held {
            return 0;
        }
        let (spread, more) = self.spread();
        more + spread.add_bound(bound, &bucket.exact)
    }

    /// An interval of an exponential grid, and the count of its own values.
    pub(super) fn join_bin(&mut self, bucket: &Open, bin: Bin, part: Part) -> usize {
        self.partial |= bucket.partial;
        self.timer |= part != Part::Own;
        let (spread, more) = self.spread();
        more + spread.add_bin(bin, &bucket.exact)
    }

    /// Its spread, and the bytes it took to make it.
    fn spread(&mut self) -> (&mut Spread, usize) {
        let more = if self.spread.is_some() { 0 } else { size_of::<Spread>() };
        (&mut **self.spread.get_or_insert_default(), more)
    }

    /// The bucket that begins at `at`, its value rounded once; none for a
    /// bucket an `op` has nothing to take from: a timer's in which it
    /// measured nothing, of `avg`, `max` or a percentile.
    pub(super) fn answer(&mut self, at: i64, shape: &Shape) -> Result<Option<Bucket>> {
        let value = match shape.op() {
            Op::Percentile(_) => self.ranked(shape)?,
            op if self.timer => self.timed(op)?.map(Value::Exact),
            op => Some(Value::Exact(self.value(op)?)),
        };
        Ok(value.map(|value| Bucket {
            at: from_unix_millis(at),
            value,
            samples: self.samples,
            resets: self.resets,
            gap: self.gap,
            partial: self.partial,
        }))
    }

    fn value(&self, op: Op) -> Result<f64> {
        Ok(match op {
            Op::Count => self.samples as f64,
            Op::Sum | Op::Increase | Op::Delta => self.exact.round(),
            Op::Avg => self.exact.divided(self.samples),
            Op::Rate => self.per_second(&self.exact),
            Op::Min => self.least,
            Op::Max => self.greatest,
            Op::First | Op::Last if self.series == 1 => self.alone,
            Op::First | Op::Last => self.exact.round(),
            Op::Percentile(_) => return Err(Error::internal("a percentile counted as one value")),
        })
    }

    /// A timer's value of `op`: none for `avg` and `max` where it measured
    /// nothing.
    fn timed(&self, op: Op) -> Result<Option<f64>> {
        let sum = || self.sum.as_deref().copied().unwrap_or(Exact::ZERO);
        Ok(match op {
            Op::Count => Some(self.exact.round()),
            Op::Rate => Some(self.per_second(&self.exact)),
            Op::Sum => Some(sum().round()),
            Op::Avg if self.samples == 0 => None,
            Op::Avg => {
                let durations = self
                    .exact
                    .whole()
                    .ok_or_else(|| Error::invalid("an average over a count of durations that is no whole number"))?;
                Some(sum().divided(durations))
            }
            Op::Max => self.longest,
            op => return Err(Error::internal(format!("{op} of a timer"))),
        })
    }

    /// A percentile: a gauge's sample at the rank, the nearest rank, or the
    /// bucket of a timer's spread that holds it.
    fn ranked(&mut self, shape: &Shape) -> Result<Option<Value>> {
        let rank = shape.rank().ok_or_else(|| Error::internal("a percentile without its rank"))?;
        if let Some(spread) = &self.spread {
            let Some((value, counted)) = spread.answer(rank, shape.estimate()) else { return Ok(None) };
            // a timer's name counts its durations by its count, a histogram's own by its +Inf
            if !(self.timer && self.series > 0) {
                self.samples = tally_count(&counted);
            }
            return Ok(Some(value));
        }
        if self.values.is_empty() {
            return Ok(None);
        }
        let position = rank.position(self.values.len() as u64) as usize;
        let (_, value, _) = self.values.select_nth_unstable_by(position - 1, f64::total_cmp);
        Ok(Some(Value::Exact(*value)))
    }

    /// `exact` a second: `exact × 1000` over the milliseconds of the bucket
    /// the range leaves.
    fn per_second(&self, exact: &Exact) -> f64 {
        exact.scaled(1000, self.end.saturating_sub(self.start).unsigned_abs())
    }
}

/// How many a counter's increase counted: the whole number it is, or the
/// nearest when it is none.
fn count_of(increase: &Exact) -> u64 {
    increase.whole().unwrap_or_else(|| increase.round() as u64)
}

fn tally_count(tally: &Tally) -> u64 {
    match tally {
        Tally::Whole(count) => *count,
        Tally::Units(units) => count_of(units),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bucket(before: Option<f64>, values: &[f64], op: Op) -> Bucket {
        let mut open = Open::new(0, (0, 60_000));
        before.into_iter().for_each(|before| open.step_from(before));
        values.iter().for_each(|value| open.add(*value, op));
        let mut joined = Joined::over(&open);
        joined.join(open, op, Part::Own);
        let shape = Shape::of(op, crate::metrics::aggregate::Asking::default()).unwrap();
        joined.answer(0, &shape).unwrap().unwrap()
    }

    fn value(before: Option<f64>, values: &[f64], op: Op) -> f64 {
        bucket(before, values, op).value.exact().unwrap()
    }

    #[test]
    fn an_increase_counts_a_reset_as_its_new_value_and_a_first_step_from_the_sample_before() {
        let alone = bucket(None, &[100.0, 110.0, 5.0, 20.0], Op::Increase);
        assert_eq!((alone.value, alone.resets, alone.samples), (Value::Exact(30.0), 1, 4));
        let stepped = bucket(Some(90.0), &[100.0, 110.0, 5.0, 20.0], Op::Increase);
        assert_eq!((stepped.value, stepped.resets), (Value::Exact(40.0), 1));
        let reset_into = bucket(Some(500.0), &[2.0, 3.0], Op::Increase);
        assert_eq!((reset_into.value, reset_into.resets), (Value::Exact(3.0), 1), "the step into the bucket fell");
        assert_eq!(value(Some(0.0), &[30.0], Op::Rate), 0.5, "30 in a minute");
    }

    #[test]
    fn a_delta_is_the_last_sample_less_the_one_before_the_first() {
        assert_eq!(value(None, &[5.0, 9.0, 7.0], Op::Delta), 2.0);
        assert_eq!(value(Some(1.0), &[5.0, 9.0, 7.0], Op::Delta), 6.0);
        assert_eq!(value(Some(7.5), &[7.0], Op::Delta), -0.5);
    }

    #[test]
    fn sums_are_exact_and_rounded_once() {
        assert_eq!(value(None, &[1e308, 1e308, -1e308], Op::Sum), 1e308, "no infinity on the way");
        assert_eq!(value(None, &[0.1, 0.2], Op::Sum), 0.1 + 0.2);
        assert_eq!(value(None, &[1.0, 2.0, 2.0], Op::Avg), 5.0 / 3.0);
        assert_eq!(value(None, &[f64::MAX, f64::MAX], Op::Sum), f64::INFINITY, "past what a float holds");
        assert_eq!(value(None, &[4.0, 4.0, 4.0], Op::Count), 3.0);
    }

    #[test]
    fn the_least_and_the_greatest_put_minus_zero_before_plus_zero_and_ends_keep_their_bits() {
        let values = [0.0, -0.0, 0.0];
        assert_eq!(value(None, &values, Op::Min).to_bits(), (-0.0_f64).to_bits());
        assert_eq!(value(None, &values, Op::Max).to_bits(), 0.0_f64.to_bits());
        assert_eq!(value(None, &[-0.0, 1.0], Op::First).to_bits(), (-0.0_f64).to_bits());
        assert_eq!(value(None, &[1.0, -0.0], Op::Last).to_bits(), (-0.0_f64).to_bits());
    }

    #[test]
    fn a_gauges_percentile_is_the_sample_at_its_nearest_rank_bit_for_bit() {
        let values: Vec<f64> = (1..=20).rev().map(f64::from).collect();
        assert_eq!(value(None, &values, Op::Percentile(95.0)), 19.0, "the 19th of 20");
        assert_eq!(value(None, &values, Op::Percentile(100.0)), 20.0);
        assert_eq!(value(None, &values, Op::Percentile(0.1)), 1.0, "the first at least");
        assert_eq!(value(None, &[0.0, -0.0], Op::Percentile(50.0)).to_bits(), (-0.0_f64).to_bits(), "-0 first");
        assert_eq!(bucket(None, &values, Op::Percentile(50.0)).samples, 20);
    }

    #[test]
    fn a_group_adds_its_series_buckets_and_rounds_once() {
        let mut group = None;
        for values in [[0.5, 0.25], [1.0, 2.0], [1e308, -1e308]] {
            let mut open = Open::new(0, (0, 60_000));
            values.iter().for_each(|value| open.add(*value, Op::Avg));
            group.get_or_insert_with(|| Joined::over(&open)).join(open, Op::Avg, Part::Own);
        }
        let shape = Shape::of(Op::Avg, crate::metrics::aggregate::Asking::default()).unwrap();
        let answer = group.unwrap().answer(0, &shape).unwrap().unwrap();
        assert_eq!((answer.value, answer.samples), (Value::Exact(0.625), 6), "every sample's sum over their count");
    }
}
