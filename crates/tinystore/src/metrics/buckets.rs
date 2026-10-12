//! A timer's or a histogram's buckets: the upper bounds of its spread, to
//! which the store adds `+Inf`, each written as the `le` label of a
//! `_bucket` series that counts what fell at most that bound; or an
//! exponential grid, whose intervals the store counts each apart, as
//! [`grid`](super::grid) says.
//!
//! A bound is written as JavaScript writes the number, so that `5` and `5.0`
//! from two languages are one series, and the digits are the fewest that
//! read back as the same number:
//!
//! ```text
//! 5.0 → 5    0.25 → 0.25    1000 → 1000    1e21 → 1e+21    1.5e-7 → 1.5e-7    ∞ → +Inf
//! ```

use crate::{Error, Result};

/// The bounds a timer or a histogram gives at most, and the intervals of a
/// grid a set of its labels counts at once at most: each is a series of
/// every set of labels it has.
pub(super) const MOST_BOUNDS: usize = 128;

/// A timer's or a histogram's buckets.
///
/// ```no_run
/// # fn f(stats: tinystore::metrics::Metrics) -> tinystore::Result<()> {
/// use tinystore::metrics::{Buckets, exponential_buckets};
/// let api = stats.timer("http_request_ms").buckets(&[5.0, 10.0, 25.0, 50.0, 100.0]).open()?;
/// let jobs = stats.timer("job_ms").buckets(exponential_buckets(1.0, 2.0, 14)).open()?;
/// let db = stats.timer("db_query_ms").buckets(Buckets::Exponential).open()?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Buckets {
    /// Upper bounds the program gives, ascending, each a number of 0 or
    /// more; the store adds `+Inf`. Each is a series counting the values at
    /// most it, `le`, as Prometheus writes a histogram's.
    Bounds(Vec<f64>),
    /// Experimental: the store counts each value in an interval of an
    /// exponential grid, Prometheus' native histograms', eight intervals a
    /// doubling, each about 9% wide, and keeps a series for an interval only
    /// once a value fell in it; past 128 intervals a set of labels it halves
    /// the resolution. A percentile then lies within one interval with no
    /// bound chosen, and two resolutions merge exactly.
    Exponential,
}

impl From<&[f64]> for Buckets {
    fn from(bounds: &[f64]) -> Buckets {
        Buckets::Bounds(bounds.to_vec())
    }
}

impl<const N: usize> From<&[f64; N]> for Buckets {
    fn from(bounds: &[f64; N]) -> Buckets {
        Buckets::Bounds(bounds.to_vec())
    }
}

impl From<Vec<f64>> for Buckets {
    fn from(bounds: Vec<f64>) -> Buckets {
        Buckets::Bounds(bounds)
    }
}

/// Buckets checked: bounds ascending, each finite and none below zero,
/// `+Inf` not among them; or the exponential grid.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Bounds {
    Given(Vec<f64>),
    Exponential,
}

impl Bounds {
    pub(super) fn of(buckets: &Buckets) -> Result<Bounds> {
        match buckets {
            Buckets::Bounds(given) => Bounds::given(given),
            Buckets::Exponential => Ok(Bounds::Exponential),
        }
    }

    /// The bounds a program gave, a `+Inf` among them left to the store.
    fn given(given: &[f64]) -> Result<Bounds> {
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
        Ok(Bounds::Given(bounds))
    }

    /// How many buckets of given bounds there are, one a bound and `+Inf`;
    /// a grid's come as values do.
    pub(super) fn buckets(&self) -> usize {
        match self {
            Bounds::Given(bounds) => bounds.len() + 1,
            Bounds::Exponential => 0,
        }
    }

    /// The `le` of each bucket of given bounds, `+Inf` last.
    pub(super) fn labels(&self) -> Vec<String> {
        match self {
            Bounds::Given(bounds) => bounds.iter().map(|bound| le(*bound)).chain(["+Inf".to_owned()]).collect(),
            Bounds::Exponential => Vec::new(),
        }
    }

    /// The buckets as a name keeps them in its row.
    pub(super) fn text(&self) -> String {
        match self {
            Bounds::Given(bounds) => {
                format!("[{}]", bounds.iter().map(|bound| le(*bound)).collect::<Vec<_>>().join(","))
            }
            Bounds::Exponential => "exponential".to_owned(),
        }
    }
}

/// The bucket of given bounds a value falls in: the first whose bound it is
/// at most, or `+Inf`'s, the last.
pub(super) fn bucket_of(bounds: &[f64], value: f64) -> usize {
    bounds.partition_point(|bound| *bound < value)
}

/// The bound an `le` gives, however a program spelt it: a number of 0 or
/// more, or `+Inf`; none for any other text.
pub(super) fn bound_of(text: &str) -> Option<f64> {
    let bound = text.parse::<f64>().ok()?;
    // -0 is the bound 0, and a NaN no bound
    (bound >= 0.0).then_some(bound + 0.0)
}

/// `count` bounds from `start`, each `factor` times the one before:
/// `exponential_buckets(1.0, 2.0, 14)` is 1, 2, 4 … 8192, bounds a program
/// gives, as Prometheus' `ExponentialBuckets` makes them;
/// [`Buckets::Exponential`] lets the store keep a finer grid instead, of
/// the intervals values fell in.
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
        let given = |bounds: &[f64]| Bounds::of(&Buckets::from(bounds));
        let bounds = given(&[5.0, 10.0, 25.0]).unwrap();
        assert_eq!(bounds.labels(), ["5", "10", "25", "+Inf"]);
        assert_eq!(bounds.text(), "[5,10,25]");
        let at = |value: f64| bucket_of(&[5.0, 10.0, 25.0], value);
        assert_eq!((at(0.0), at(5.0), at(5.5), at(25.0), at(26.0)), (0, 0, 1, 2, 3));
        assert_eq!(given(&[1.0, f64::INFINITY]).unwrap().labels(), ["1", "+Inf"], "an infinity given is the store's");
        for refused in [&[][..], &[3.0, 2.0], &[1.0, 1.0], &[-1.0, 1.0], &[f64::NAN], &[f64::INFINITY]] {
            assert!(given(refused).is_err(), "{refused:?}");
        }
        assert!(given(&exponential_buckets(1.0, 2.0, MOST_BOUNDS + 1)).is_err());
        assert_eq!(exponential_buckets(1.0, 2.0, 14).last(), Some(&8192.0));
        let grid = Bounds::of(&Buckets::Exponential).unwrap();
        assert_eq!((grid.buckets(), grid.labels(), grid.text()), (0, Vec::<String>::new(), "exponential".to_owned()));
    }

    #[test]
    fn a_bound_reads_however_a_program_spelt_it() {
        let cases = [("5", Some(5.0)), ("5.0", Some(5.0)), ("+Inf", Some(f64::INFINITY)), ("-0", Some(0.0))];
        for (text, bound) in cases {
            assert_eq!(bound_of(text), bound, "{text}");
        }
        assert_eq!(bound_of("-0").map(f64::to_bits), Some(0), "the bound 0, not -0");
        for refused in ["", "five", "-1", "NaN", "-Inf", "5 "] {
            assert_eq!(bound_of(refused), None, "{refused}");
        }
    }
}
