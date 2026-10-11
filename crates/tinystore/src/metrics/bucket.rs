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
//!
//! A step is counted in the bucket it ends in: a bucket's first step begins
//! at the sample before the bucket, which the series carries into it.
//!
//! ```text
//! 100 → 110 │ 5 → 20        increases 10 │ 5 + 15, a reset between them
//! ```

use super::aggregate::{Bucket, Op};
use super::directory::Block;
use super::exact::{BLOCK_BITS, Exact};
use super::summary::{signed_greatest, signed_least};
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
    samples: u64,
    resets: u64,
    least: f64,
    greatest: f64,
    first: f64,
    last: f64,
    /// The exact sum of its samples, or of its steps.
    exact: Exact,
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
            before: None,
            gap: false,
            partial: false,
        }
    }

    /// Begins its first step at the sample before the bucket.
    pub(super) fn step_from(&mut self, before: f64) {
        self.before = Some(before);
    }

    /// One sample more, a finite one.
    pub(super) fn add(&mut self, value: f64, op: Op) {
        self.widen(value, value, value);
        match op {
            Op::Sum | Op::Avg => {
                self.exact.add_float(value);
            }
            Op::Increase | Op::Rate => self.step(value),
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
#[derive(Clone, Debug)]
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
        }
    }

    /// Adds a series' closed bucket.
    pub(super) fn join(&mut self, bucket: &Open, op: Op) {
        self.least = signed_least(self.least, bucket.least);
        self.greatest = signed_greatest(self.greatest, bucket.greatest);
        self.alone = if op == Op::First { bucket.first } else { bucket.last };
        self.exact.add(&bucket.total(op));
        self.samples += bucket.samples;
        self.resets += bucket.resets;
        self.series += 1;
        self.gap |= bucket.gap;
        self.partial |= bucket.partial;
    }

    /// The bucket that begins at `at`, its value rounded once.
    pub(super) fn answer(&self, at: i64, op: Op) -> Bucket {
        let value = match op {
            Op::Count => self.samples as f64,
            Op::Sum | Op::Increase | Op::Delta => self.exact.round(),
            Op::Avg => self.exact.divided(self.samples),
            // a second of what the range leaves of the bucket, in milliseconds
            Op::Rate => self.exact.scaled(1000, self.end.saturating_sub(self.start).unsigned_abs()),
            Op::Min => self.least,
            Op::Max => self.greatest,
            Op::First | Op::Last if self.series == 1 => self.alone,
            Op::First | Op::Last => self.exact.round(),
        };
        Bucket {
            at: from_unix_millis(at),
            value,
            samples: self.samples,
            resets: self.resets,
            gap: self.gap,
            partial: self.partial,
        }
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
        joined.join(&open, op);
        joined.answer(0, op)
    }

    #[test]
    fn an_increase_counts_a_reset_as_its_new_value_and_a_first_step_from_the_sample_before() {
        let alone = bucket(None, &[100.0, 110.0, 5.0, 20.0], Op::Increase);
        assert_eq!((alone.value, alone.resets, alone.samples), (30.0, 1, 4));
        let stepped = bucket(Some(90.0), &[100.0, 110.0, 5.0, 20.0], Op::Increase);
        assert_eq!((stepped.value, stepped.resets), (40.0, 1));
        let reset_into = bucket(Some(500.0), &[2.0, 3.0], Op::Increase);
        assert_eq!((reset_into.value, reset_into.resets), (3.0, 1), "the step into the bucket fell");
        assert_eq!(bucket(Some(0.0), &[30.0], Op::Rate).value, 0.5, "30 in a minute");
    }

    #[test]
    fn a_delta_is_the_last_sample_less_the_one_before_the_first() {
        assert_eq!(bucket(None, &[5.0, 9.0, 7.0], Op::Delta).value, 2.0);
        assert_eq!(bucket(Some(1.0), &[5.0, 9.0, 7.0], Op::Delta).value, 6.0);
        assert_eq!(bucket(Some(7.5), &[7.0], Op::Delta).value, -0.5);
    }

    #[test]
    fn sums_are_exact_and_rounded_once() {
        assert_eq!(bucket(None, &[1e308, 1e308, -1e308], Op::Sum).value, 1e308, "no infinity on the way");
        assert_eq!(bucket(None, &[0.1, 0.2], Op::Sum).value, 0.1 + 0.2);
        assert_eq!(bucket(None, &[1.0, 2.0, 2.0], Op::Avg).value, 5.0 / 3.0);
        assert_eq!(bucket(None, &[f64::MAX, f64::MAX], Op::Sum).value, f64::INFINITY, "past what a float holds");
        assert_eq!(bucket(None, &[4.0, 4.0, 4.0], Op::Count).value, 3.0);
    }

    #[test]
    fn the_least_and_the_greatest_put_minus_zero_before_plus_zero_and_ends_keep_their_bits() {
        let values = [0.0, -0.0, 0.0];
        assert_eq!(bucket(None, &values, Op::Min).value.to_bits(), (-0.0_f64).to_bits());
        assert_eq!(bucket(None, &values, Op::Max).value.to_bits(), 0.0_f64.to_bits());
        assert_eq!(bucket(None, &[-0.0, 1.0], Op::First).value.to_bits(), (-0.0_f64).to_bits());
        assert_eq!(bucket(None, &[1.0, -0.0], Op::Last).value.to_bits(), (-0.0_f64).to_bits());
    }

    #[test]
    fn a_group_adds_its_series_buckets_and_rounds_once() {
        let mut group = None;
        for values in [[0.5, 0.25], [1.0, 2.0], [1e308, -1e308]] {
            let mut open = Open::new(0, (0, 60_000));
            values.iter().for_each(|value| open.add(*value, Op::Avg));
            group.get_or_insert_with(|| Joined::over(&open)).join(&open, Op::Avg);
        }
        let answer = group.unwrap().answer(0, Op::Avg);
        assert_eq!((answer.value, answer.samples), (0.625, 6), "every sample's sum over every sample's count");
    }
}
