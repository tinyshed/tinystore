//! A block's summary, kept in its group's directory: what an aggregate takes
//! from a block wholly inside one bucket without decoding it.
//!
//! The exact sum and increase are there only when every sample is finite, and
//! a counter's not below zero; the floats beside them are the Go engine's
//! format, which no read of this one takes a sum from.

use super::codec::Point;
use super::exact::Exact;
use super::series::Kind;

#[derive(Clone, Debug)]
pub(super) struct Summary {
    pub(super) last: f64,
    pub(super) min: f64,
    pub(super) max: f64,
    /// A gauge's samples added as floats, in their order.
    pub(super) sum: f64,
    /// A counter's growth as floats, a reset counting its new value.
    pub(super) increase: f64,
    pub(super) resets: u16,
    /// No sample is a NaN or an infinity, nor a counter's below zero.
    pub(super) valid: bool,
    /// The exact sum and increase, as a directory spells them.
    pub(super) exact: Option<Sums>,
}

/// A block's exact sum and increase, each in its canonical bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Sums {
    pub(super) sum: Vec<u8>,
    pub(super) increase: Vec<u8>,
}

/// The summary of a block's samples, in the order of their times.
pub(super) fn summarize(points: &[Point], kind: Kind) -> Summary {
    let first = points[0].value;
    let counts = kind != Kind::Gauge;
    let mut summary = Summary {
        last: points[points.len() - 1].value,
        min: first,
        max: first,
        sum: 0.0,
        increase: 0.0,
        resets: 0,
        valid: true,
        exact: None,
    };
    let mut previous = first;
    for point in points {
        let value = point.value;
        summary.valid &= value.is_finite() && !(counts && value < 0.0);
        if counts {
            if value < previous {
                summary.resets += 1;
                summary.increase += value;
            } else {
                summary.increase += value - previous;
            }
        } else {
            summary.min = least(summary.min, value);
            summary.max = greatest(summary.max, value);
            summary.sum += value;
        }
        previous = value;
    }
    summary.valid &= summary.sum.is_finite() && summary.increase.is_finite();
    exact(points, counts, &mut summary);
    summary
}

/// Adds the exact sum and increase, and the least and greatest sample with
/// `-0` below `+0`, when every sample is finite and a counter's not below
/// zero.
fn exact(points: &[Point], counts: bool, summary: &mut Summary) {
    let (mut sum, mut increase) = (Exact::ZERO, Exact::ZERO);
    let (mut min, mut max) = (points[0].value, points[0].value);
    let mut previous = points[0].value;
    for point in points {
        let value = point.value;
        if !value.is_finite() || (counts && value < 0.0) {
            return;
        }
        sum.add_float(value);
        if value < previous {
            increase.add_float(value);
        } else {
            increase.add_float(value);
            increase.sub_float(previous);
        }
        (min, max, previous) = (signed_least(min, value), signed_greatest(max, value), value);
    }
    if !counts {
        increase = Exact::ZERO;
    }
    let (mut sum_bytes, mut increase_bytes) = (Vec::new(), Vec::new());
    sum.put(&mut sum_bytes);
    increase.put(&mut increase_bytes);
    (summary.min, summary.max) = (min, max);
    summary.exact = Some(Sums { sum: sum_bytes, increase: increase_bytes });
}

/// The less of two, a NaN taking over as Go's `math.Min` lets it.
fn least(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { signed_least(a, b) }
}

fn greatest(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { signed_greatest(a, b) }
}

/// The less of two, `-0` below `+0`.
pub(super) fn signed_least(a: f64, b: f64) -> f64 {
    if b < a || (b == 0.0 && a == 0.0 && b.is_sign_negative()) { b } else { a }
}

/// The greater of two, `+0` above `-0`.
pub(super) fn signed_greatest(a: f64, b: f64) -> f64 {
    if b > a || (b == 0.0 && a == 0.0 && b.is_sign_positive()) { b } else { a }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::varint::Reader;
    use crate::metrics::exact::BLOCK_BITS;

    fn points(values: &[f64]) -> Vec<Point> {
        values.iter().zip(0..).map(|(&value, at)| Point { at, value }).collect()
    }

    fn exact(bytes: &[u8]) -> f64 {
        Exact::read(&mut Reader::new(bytes), BLOCK_BITS).round()
    }

    #[test]
    fn a_counters_increase_counts_a_reset_as_its_new_value() {
        let summary = summarize(&points(&[100.0, 110.0, 5.0, 20.0]), Kind::Counter);
        assert_eq!((summary.resets, summary.increase, summary.last), (1, 30.0, 20.0));
        let sums = summary.exact.unwrap();
        assert_eq!((exact(&sums.sum), exact(&sums.increase)), (235.0, 30.0));
        assert!(summary.valid);
    }

    #[test]
    fn a_gauges_least_and_greatest_put_minus_zero_below_plus_zero() {
        let summary = summarize(&points(&[0.0, -0.0, 3.0]), Kind::Gauge);
        assert_eq!((summary.min.to_bits(), summary.max), ((-0.0_f64).to_bits(), 3.0));
        assert_eq!(exact(&summary.exact.unwrap().increase), 0.0, "a gauge keeps no increase");
    }

    #[test]
    fn a_block_of_a_nan_an_infinity_or_a_counter_below_zero_has_no_exact_sums() {
        for (values, kind) in [
            (vec![1.0, f64::NAN], Kind::Gauge),
            (vec![1.0, f64::INFINITY], Kind::Gauge),
            (vec![1.0, -1.0], Kind::Counter),
        ] {
            let summary = summarize(&points(&values), kind);
            assert!(!summary.valid && summary.exact.is_none(), "{values:?}");
        }
        let overflowing = summarize(&points(&[f64::MAX, f64::MAX]), Kind::Gauge);
        assert!(!overflowing.valid, "a float sum past the largest float");
        assert!(overflowing.exact.is_some(), "an exact sum has no largest");
    }
}
