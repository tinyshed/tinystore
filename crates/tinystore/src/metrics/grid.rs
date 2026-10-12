//! The exponential grid of the buckets a timer lets the store keep:
//! Prometheus' native histograms', whose bounds are 2^(i/2^schema) for every
//! integer i, an interval (bound(i − 1), bound(i)] holding what lies above
//! its lower bound and at most its upper.
//!
//! ```text
//! schema 3   … 1  1.0905  1.1892  1.2968  1.4142 …   eight intervals a doubling, each about 9% wide
//! schema 2   … 1          1.1892          1.4142 …   every other bound, the same floats
//! ```
//!
//! A coarser schema's bounds are every other of the finer's, the same floats,
//! so interval i of the finer lies whole in interval ⌈i/2⌉ of the coarser and
//! two resolutions merge exactly.
//!
//! The bounds come from Prometheus' table and never from `pow`: three of them
//! lie a unit in the last place below the float nearest 2^(j/8), and a value
//! on a bound must fall where Prometheus puts it, so that a native histogram
//! merges with these exactly.

/// The finest schema an instrument writes: eight intervals a doubling.
pub(super) const FINEST: u8 = 3;

/// 2^(j/8 − 1) for j in 0..8, as Prometheus' `exponentialBounds` writes them
/// for schema 3: a bound's fraction, in [0.5, 1). A coarser schema takes
/// every other.
const FRACTIONS: [f64; 8] = [
    0.5,
    0.545_253_866_332_628_8,
    0.594_603_557_501_360_5,
    0.648_419_777_325_504_8,
    0.707_106_781_186_547_5,
    0.771_105_412_703_970_4,
    0.840_896_415_253_714_4,
    0.917_004_043_204_671_1,
];

/// An interval of a grid: what a value fell in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Bin {
    /// Exactly 0.
    Zero,
    /// (bound(index − 1), bound(index)] at `schema`; the lowest index holds
    /// every value above 0 up to the least normal float.
    At { schema: u8, index: i32 },
}

impl Bin {
    /// The interval of `schema` a value of 0 or more falls in.
    ///
    /// ```text
    /// schema 3   1 → 0    1.05 → 1    0.99 → 0    1.414213562373095 → 4, on its bound
    /// ```
    pub(super) fn of(value: f64, schema: u8) -> Bin {
        if value == 0.0 {
            return Bin::Zero;
        }
        // a subnormal falls in the lowest interval, whose bound is the least normal float
        let (fraction, exponent) = split(value.max(f64::MIN_POSITIVE));
        let count = 1_usize << schema;
        let stride = 1 << (FINEST - schema);
        let within = (0..count).position(|place| FRACTIONS[place * stride] >= fraction).unwrap_or(count);
        Bin::At { schema, index: within as i32 + (exponent - 1) * count as i32 }
    }

    /// The same values' interval at a coarser schema, which holds it whole.
    pub(super) fn at(self, coarser: u8) -> Bin {
        match self {
            Bin::At { schema, index } if schema > coarser => {
                let width = 1_i32 << (schema - coarser);
                Bin::At { schema: coarser, index: -(-index).div_euclid(width) }
            }
            bin => bin,
        }
    }

    pub(super) fn schema(self) -> Option<u8> {
        match self {
            Bin::Zero => None,
            Bin::At { schema, .. } => Some(schema),
        }
    }

    /// Its lower bound, not in it, and its upper bound, in it: 0 and 0 for
    /// [`Bin::Zero`], which holds 0 alone.
    pub(super) fn bounds(self) -> (f64, f64) {
        match self {
            Bin::Zero => (0.0, 0.0),
            Bin::At { schema, index } if index <= lowest(schema) => (0.0, upper(schema, lowest(schema))),
            Bin::At { schema, index } => (upper(schema, index - 1), upper(schema, index)),
        }
    }

    /// As the label `__bin` writes it: the schema and the index, `3:13`, or
    /// `zero`.
    pub(super) fn label(self) -> String {
        match self {
            Bin::Zero => "zero".to_owned(),
            Bin::At { schema, index } => format!("{schema}:{index}"),
        }
    }

    /// What [`Bin::label`] wrote; none for any other text.
    pub(super) fn parse(text: &str) -> Option<Bin> {
        if text == "zero" {
            return Some(Bin::Zero);
        }
        let (schema, index) = text.split_once(':')?;
        let (schema, index) = (schema.parse::<u8>().ok()?, index.parse::<i32>().ok()?);
        let highest = 1024_i32 << schema.min(FINEST);
        let within = schema <= FINEST && (lowest(schema)..=highest).contains(&index);
        // the text must be the one spelling of its numbers
        (within && Bin::At { schema, index }.label() == text).then_some(Bin::At { schema, index })
    }
}

/// The index of a schema's lowest interval: its bound is the least normal
/// float, 2^−1022.
fn lowest(schema: u8) -> i32 {
    -1022 << schema
}

/// The upper bound of interval `index` at `schema`; the largest float for the
/// last, whose bound 2^1024 a float cannot hold, as Prometheus keeps it.
fn upper(schema: u8, index: i32) -> f64 {
    let count = 1_i32 << schema;
    let fraction = FRACTIONS[(index.rem_euclid(count) as usize) << (FINEST - schema)];
    let bound = times_two_to(fraction, index.div_euclid(count) + 1);
    if bound.is_infinite() { f64::MAX } else { bound }
}

/// A positive normal float as a fraction in [0.5, 1) and a power of two, as
/// C's `frexp` gives them.
fn split(value: f64) -> (f64, i32) {
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1022;
    let fraction = f64::from_bits((bits & !(0x7ff << 52)) | (1022 << 52));
    (fraction, exponent)
}

/// `value` × 2^`power`, exact while the result is a normal float: two steps,
/// each a power of two a float holds.
fn times_two_to(value: f64, power: i32) -> f64 {
    let half = power / 2;
    value * two_to(half) * two_to(power - half)
}

/// 2^`power` for a power a normal float holds.
fn two_to(power: i32) -> f64 {
    f64::from_bits(((power + 1023) as u64) << 52)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values on bounds, a unit in the last place either side of them, and
    /// between them, across the range a float holds.
    fn values() -> Vec<f64> {
        let mut values = vec![f64::MIN_POSITIVE, 5e-324, 1e-310, f64::MAX, 1.0, 1.05, 0.99, 3.0, 1e-6, 86_400_000.0];
        for index in [-8176, -8000, -100, -9, -1, 0, 1, 7, 8, 13, 100, 8000, 8191] {
            let bound = upper(FINEST, index);
            values.extend([bound, f64::from_bits(bound.to_bits() - 1), f64::from_bits(bound.to_bits() + 1)]);
        }
        values.retain(|value| value.is_finite() && *value > 0.0);
        values
    }

    #[test]
    fn a_value_falls_in_the_interval_whose_bounds_hold_it() {
        assert_eq!(Bin::of(0.0, FINEST), Bin::Zero);
        assert_eq!(Bin::of(-0.0, FINEST), Bin::Zero);
        for schema in 0..=FINEST {
            for value in values() {
                let bin = Bin::of(value, schema);
                let (lower, upper) = bin.bounds();
                assert!(lower < value && value <= upper, "{value:e} at {schema}: {bin:?} is ({lower:e}, {upper:e}]");
            }
        }
    }

    #[test]
    fn a_value_falls_where_prometheus_puts_it() {
        let at = |index| Bin::At { schema: FINEST, index };
        assert_eq!((Bin::of(1.0, 3), Bin::of(1.05, 3), Bin::of(0.99, 3)), (at(0), at(1), at(0)));
        // Prometheus' bound for √2 lies a unit in the last place below the float nearest it
        let below = f64::from_bits(std::f64::consts::SQRT_2.to_bits() - 1);
        assert_eq!((Bin::of(below, 3), Bin::of(std::f64::consts::SQRT_2, 3)), (at(4), at(5)));
        assert_eq!(Bin::of(f64::MAX, 3), at(8192), "the last interval ends at the largest float");
        assert_eq!(at(8192).bounds().1, f64::MAX);
    }

    #[test]
    fn a_coarser_grid_takes_every_other_bound_so_two_resolutions_merge_exactly() {
        for schema in 1..=FINEST {
            for index in [-8176, -77, -2, -1, 0, 1, 2, 3, 77, 8000] {
                let index = index >> (FINEST - schema);
                let (fine, coarse) = (upper(schema, 2 * index), upper(schema - 1, index));
                assert_eq!(fine.to_bits(), coarse.to_bits(), "bound {index} at {}", schema - 1);
            }
            for value in values() {
                assert_eq!(Bin::of(value, schema).at(schema - 1), Bin::of(value, schema - 1), "{value:e}");
                assert_eq!(Bin::of(value, schema).at(0), Bin::of(value, 0), "{value:e}");
            }
        }
    }

    #[test]
    fn each_bound_lies_within_a_unit_in_the_last_place_of_its_power_of_two() {
        for (place, fraction) in FRACTIONS.iter().enumerate() {
            let exact = 2_f64.powf(place as f64 / 8.0) / 2.0;
            assert!((fraction - exact).abs() <= f64::EPSILON, "{place}: {fraction} beside {exact}");
        }
    }

    #[test]
    fn a_label_reads_back_as_its_interval_and_no_other_text_does() {
        for bin in [Bin::Zero, Bin::At { schema: 3, index: 13 }, Bin::At { schema: 0, index: -1022 }] {
            assert_eq!(Bin::parse(&bin.label()), Some(bin));
        }
        for text in ["", "0", "3", "3:", ":1", "4:1", "3:+1", "3:01", "03:1", "3:8193", "3:-8177", "Zero"] {
            assert_eq!(Bin::parse(text), None, "{text}");
        }
    }
}
