//! A timer's or a histogram's buckets: the upper bounds of its spread, to
//! which the store adds `+Inf`, each written as the `le` label of a
//! `_bucket` series that counts what fell at most that bound.
//!
//! A bound is written as JavaScript writes the number, so that `5` and `5.0`
//! from two languages are one series, and the digits are the fewest that
//! read back as the same number:
//!
//! ```text
//! 5.0 → 5    0.25 → 0.25    1000 → 1000    1e21 → 1e+21    1.5e-7 → 1.5e-7    ∞ → +Inf
//! ```

use crate::{Error, Result};

/// The bounds a timer or a histogram gives at most: each is a series of
/// every set of labels it has.
pub(super) const MOST_BOUNDS: usize = 128;

/// Bounds checked: ascending, each finite and none below zero, `+Inf` not
/// among them.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Bounds(Vec<f64>);

impl Bounds {
    /// The bounds a program gave, a `+Inf` among them left to the store.
    pub(super) fn of(given: &[f64]) -> Result<Bounds> {
        let bounds: Vec<f64> = given.iter().copied().filter(|bound| *bound != f64::INFINITY).collect();
        if bounds.is_empty() || bounds.len() > MOST_BOUNDS {
            return Err(Error::invalid(format!("buckets: 1 to {MOST_BOUNDS} bounds, `+Inf` added by the store")));
        }
        if let Some(bound) = bounds.iter().find(|bound| !bound.is_finite() || **bound < 0.0) {
            return Err(Error::invalid(format!("buckets: a bound of {bound}, where a bound is a number of 0 or more")));
        }
        if bounds.windows(2).any(|pair| pair[1] <= pair[0]) {
            return Err(Error::invalid("buckets: bounds in ascending order, each once"));
        }
        Ok(Bounds(bounds))
    }

    /// How many buckets they make: one a bound, and `+Inf`.
    pub(super) fn buckets(&self) -> usize {
        self.0.len() + 1
    }

    /// The bucket a value falls in: the first whose bound it is at most, or
    /// `+Inf`'s, the last.
    pub(super) fn bucket_of(&self, value: f64) -> usize {
        self.0.partition_point(|bound| *bound < value)
    }

    /// The `le` of each bucket, `+Inf` last.
    pub(super) fn labels(&self) -> Vec<String> {
        self.0.iter().map(|bound| le(*bound)).chain(["+Inf".to_owned()]).collect()
    }

    /// The bounds as a name keeps them in its row.
    pub(super) fn text(&self) -> String {
        format!("[{}]", self.0.iter().map(|bound| le(*bound)).collect::<Vec<_>>().join(","))
    }
}

/// `count` bounds from `start`, each `factor` times the one before:
/// `exponential_buckets(1.0, 2.0, 14)` is 1, 2, 4 … 8192.
pub fn exponential_buckets(start: f64, factor: f64, count: usize) -> Vec<f64> {
    std::iter::successors(Some(start), |bound| Some(bound * factor)).take(count).collect()
}

/// A bound as its `le` label: as JavaScript's `String(n)` writes it, the
/// fewest digits that read back as the number, decimal from a millionth to
/// 10²¹ and with an exponent beyond.
pub(super) fn le(bound: f64) -> String {
    if bound == f64::INFINITY {
        return "+Inf".to_owned();
    }
    if bound == 0.0 {
        return "0".to_owned();
    }
    // Rust's exponent form gives the same fewest digits: 1.2345e3, 1e-7
    let scientific = format!("{bound:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let point = exponent.parse::<i32>().unwrap_or(0) + 1;
    let count = digits.len() as i32;
    match point {
        point if count <= point && point <= 21 => digits + &"0".repeat((point - count) as usize),
        point if 0 < point && point <= 21 => format!("{}.{}", &digits[..point as usize], &digits[point as usize..]),
        point if -6 < point && point <= 0 => format!("0.{}{digits}", "0".repeat(point.unsigned_abs() as usize)),
        point => {
            let power = point - 1;
            let sign = if power < 0 { '-' } else { '+' };
            let (first, rest) = digits.split_at(1);
            let fraction = if rest.is_empty() { String::new() } else { format!(".{rest}") };
            format!("{first}{fraction}e{sign}{}", power.unsigned_abs())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bound_is_written_as_javascript_writes_the_number() {
        let cases = [
            (5.0, "5"),
            (0.25, "0.25"),
            (2.5, "2.5"),
            (1000.0, "1000"),
            (0.1, "0.1"),
            (123_456_789.125, "123456789.125"),
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
            (1.5e21, "1.5e+21"),
            (0.000_001, "0.000001"),
            (1e-7, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (-0.0, "0"),
            (f64::INFINITY, "+Inf"),
        ];
        for (bound, text) in cases {
            assert_eq!(le(bound), text, "{bound:e}");
            if bound.is_finite() {
                assert_eq!(text.parse::<f64>().unwrap(), bound, "it reads back");
            }
        }
    }

    #[test]
    fn bounds_ascend_from_zero_and_the_store_adds_infinity() {
        let bounds = Bounds::of(&[5.0, 10.0, 25.0]).unwrap();
        assert_eq!(bounds.labels(), ["5", "10", "25", "+Inf"]);
        assert_eq!(bounds.text(), "[5,10,25]");
        let at = |value: f64| bounds.bucket_of(value);
        assert_eq!((at(0.0), at(5.0), at(5.5), at(25.0), at(26.0)), (0, 0, 1, 2, 3));
        assert_eq!(
            Bounds::of(&[1.0, f64::INFINITY]).unwrap().labels(),
            ["1", "+Inf"],
            "an infinity given is the store's"
        );
        for refused in [&[][..], &[3.0, 2.0], &[1.0, 1.0], &[-1.0, 1.0], &[f64::NAN], &[f64::INFINITY]] {
            assert!(Bounds::of(refused).is_err(), "{refused:?}");
        }
        assert!(Bounds::of(&exponential_buckets(1.0, 2.0, MOST_BOUNDS + 1)).is_err());
        assert_eq!(exponential_buckets(1.0, 2.0, 14).last(), Some(&8192.0));
    }
}
