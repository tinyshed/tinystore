//! A percentile's rank, and the exact counts it is found among.
//!
//! A rank is the decimal the query wrote, `p99.9` as 999 in 1000, so that no
//! float's rounding decides which sample or bucket holds it: 0.07 × 100 is
//! 7.000000000000001 as floats, and its ceiling would take the eighth sample
//! where the seventh holds the rank.

use super::exact::Exact;
use crate::{Error, Result};

/// The decimals a rank has at most: `p99.999999999999999` and no finer.
const MOST_DECIMALS: u32 = 15;

/// A rank above 0 and at most 1, as a fraction: `parts` of `whole`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rank {
    parts: u64,
    whole: u64,
}

impl Rank {
    /// The rank of `percent`, a number above 0 and at most 100, as the
    /// fewest digits that read back as it give it: `99.9` is 999 in 1000.
    pub(super) fn of(percent: f64) -> Result<Rank> {
        if !(percent > 0.0 && percent <= 100.0) {
            return Err(Error::invalid(format!("p{percent}: a percentile above 0 and at most 100")));
        }
        // Rust writes a float's fewest digits, and never with an exponent
        let text = percent.to_string();
        let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
        let decimals = fraction.len() as u32;
        if decimals > MOST_DECIMALS {
            return Err(Error::invalid(format!("p{percent}: a percentile of {MOST_DECIMALS} decimals at most")));
        }
        let parts =
            format!("{whole}{fraction}").parse::<u64>().map_err(|_| Error::internal("a percentile's digits"))?;
        Ok(Rank { parts, whole: 10_u64.pow(decimals + 2) })
    }

    /// Where the sample at this rank stands among `count`, from 1: the
    /// nearest rank, ⌈rank × count⌉, and 1 at least.
    pub(super) fn position(self, count: u64) -> u64 {
        let position = (u128::from(self.parts) * u128::from(count)).div_ceil(u128::from(self.whole));
        u64::try_from(position).unwrap_or(u64::MAX).max(1)
    }

    /// Whether `count` values reach this rank of `total`: `count` ≥ rank ×
    /// `total`, compared exactly.
    pub(super) fn reached(self, count: &Tally, total: &Tally) -> bool {
        match (count, total) {
            (Tally::Whole(count), Tally::Whole(total)) => {
                u128::from(*count) * u128::from(self.whole) >= u128::from(*total) * u128::from(self.parts)
            }
            _ => count.exact().times(self.whole) >= total.exact().times(self.parts),
        }
    }

    /// The rank as a float, for an estimate, which is no exact answer.
    pub(super) fn approximately(self) -> f64 {
        self.parts as f64 / self.whole as f64
    }
}

/// A count, exact: a whole number while every count added is one, as every
/// count an instrument writes is, and every unit of it once one is not.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Tally {
    Whole(u64),
    Units(Box<Exact>),
}

impl Default for Tally {
    fn default() -> Tally {
        Tally::Whole(0)
    }
}

impl Tally {
    pub(super) fn add(&mut self, more: &Exact) {
        if let (Tally::Whole(count), Some(whole)) = (&mut *self, more.whole())
            && let Some(sum) = count.checked_add(whole)
        {
            *count = sum;
            return;
        }
        let mut units = self.exact();
        units.add(more);
        *self = Tally::Units(Box::new(units));
    }

    pub(super) fn add_tally(&mut self, more: &Tally) {
        self.add(&more.exact());
    }

    pub(super) fn exact(&self) -> Exact {
        match self {
            Tally::Whole(count) => Exact::of_whole(*count),
            Tally::Units(units) => **units,
        }
    }

    pub(super) fn is_zero(&self) -> bool {
        match self {
            Tally::Whole(count) => *count == 0,
            Tally::Units(units) => units.whole() == Some(0),
        }
    }

    /// The count as the nearest float.
    pub(super) fn round(&self) -> f64 {
        match self {
            Tally::Whole(count) => *count as f64,
            Tally::Units(units) => units.round(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rank_is_the_decimal_the_query_wrote_and_no_float_rounds_it() {
        let rank = |percent| Rank::of(percent).unwrap();
        assert_eq!(rank(95.0), Rank { parts: 95, whole: 100 });
        assert_eq!(rank(99.9), Rank { parts: 999, whole: 1000 });
        assert_eq!(rank(0.000_000_1), Rank { parts: 1, whole: 1_000_000_000 });
        // 0.07 × 100 is 7.000000000000001 as floats, whose ceiling is 8
        assert_eq!(rank(7.0).position(100), 7);
        assert_eq!((rank(95.0).position(20), rank(95.0).position(21), rank(100.0).position(21)), (19, 20, 21));
        assert_eq!(rank(0.001).position(3), 1, "the first sample at least");
        for refused in [0.0, -1.0, 100.5, f64::NAN, f64::INFINITY, 1e-16] {
            assert!(Rank::of(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn a_count_reaches_a_rank_compared_exactly_whole_or_not() {
        let whole = |count| Tally::Whole(count);
        let rank = Rank::of(95.0).unwrap();
        assert!(rank.reached(&whole(95), &whole(100)));
        assert!(!rank.reached(&whole(94), &whole(100)));
        let mut counted = Tally::default();
        counted.add(&Exact::of_whole(90));
        counted.add_tally(&whole(4));
        assert_eq!(counted, whole(94), "whole counts stay whole");
        let mut units = whole(94);
        let mut point_five = Exact::ZERO;
        point_five.add_float(0.5);
        units.add(&point_five);
        assert!(matches!(units, Tally::Units(_)));
        assert!(!rank.reached(&units, &whole(100)), "94.5 is below 95");
        units.add(&point_five);
        assert!(rank.reached(&units, &whole(100)), "95, though counted in units");
        assert_eq!((units.round(), units.is_zero(), Tally::default().is_zero()), (95.0, false, true));
    }

    #[test]
    fn a_whole_count_reads_back_and_a_fraction_or_a_negative_is_none() {
        for count in [0, 1, 7, 1 << 53, u64::MAX] {
            assert_eq!(Exact::of_whole(count).whole(), Some(count));
        }
        let mut fraction = Exact::of_whole(3);
        fraction.add_float(0.25);
        let mut negative = Exact::ZERO;
        negative.sub_float(2.0);
        let mut past = Exact::of_whole(u64::MAX);
        past.add(&Exact::of_whole(1));
        assert_eq!((fraction.whole(), negative.whole(), past.whole()), (None, None, None));
        assert_eq!(Exact::of_whole(1 << 60).times(1000).round(), (1_u64 << 60) as f64 * 1000.0);
    }
}
