//! A timer's percentile in one bucket of time: the counts of its buckets,
//! each member's, joined across a group, and the bucket of its spread that
//! holds the rank.
//!
//! A member is one set of a timer's labels. Its writer writes every bound of
//! its set at each write, so in a bucket of time a member holds the bounds
//! whose series counted as many samples as its `+Inf`: a bound of a set
//! written for part of that time counts fewer. The group keeps the bounds
//! every member holds, `+Inf` among them, adds their counts exactly, and
//! finds the rank among them.
//!
//! ```text
//! one member's hour   +Inf 5 samples   [10, 20, 50, 100] 3 each, then [5, 25, 75, 150] 2 each
//! the bounds it holds there: +Inf alone, so the rank lies above 0 and at most Infinity
//! ```
//!
//! An exponential grid's intervals each count their own values: they join at
//! the coarsest schema among them, whose bounds a finer schema shares, and
//! beside bounds a program gave, the rank is found among the bounds both have.

use std::collections::BTreeMap;

use super::aggregate::Value;
use super::exact::Exact;
use super::grid::{Bin, FINEST};
use super::rank::{Rank, Tally};

/// What a bound or an interval of a spread holds of the store's memory.
pub(super) const ENTRY_HELD: usize = 64;

/// A timer's buckets in one bucket of time, across a group.
#[derive(Debug, Default)]
pub(super) struct Spread {
    /// By bound, keyed by its bits, which order as bounds of 0 or more do:
    /// the count at most it of the members that hold it, and how many do.
    bounds: BTreeMap<u64, Held>,
    /// The members with a `+Inf` here.
    members: u32,
    bins: BTreeMap<Bin, Tally>,
}

#[derive(Debug, Default)]
struct Held {
    count: Tally,
    members: u32,
}

/// A step of the ladder a rank is found on: the values above `lower` and at
/// most `upper`, and the count of every value at most `upper`.
#[derive(Debug)]
struct Step {
    lower: f64,
    upper: f64,
    count: Tally,
}

impl Spread {
    /// A member's `+Inf`, which counts every value of the member; gives the
    /// bytes it holds more.
    pub(super) fn add_infinity(&mut self, count: &Exact) -> usize {
        self.members += 1;
        self.add_bound(f64::INFINITY, count)
    }

    /// A bound a member holds here, and its count of the values at most it.
    pub(super) fn add_bound(&mut self, bound: f64, count: &Exact) -> usize {
        let key = (bound + 0.0).to_bits();
        let new = !self.bounds.contains_key(&key);
        let held = self.bounds.entry(key).or_default();
        held.count.add(count);
        held.members += 1;
        if new { ENTRY_HELD } else { 0 }
    }

    /// An interval of an exponential grid, and its count of its own values.
    pub(super) fn add_bin(&mut self, bin: Bin, count: &Exact) -> usize {
        let new = !self.bins.contains_key(&bin);
        self.bins.entry(bin).or_default().add(count);
        if new { ENTRY_HELD } else { 0 }
    }

    /// The step of the spread that holds `rank`, and how many values the
    /// spread counted; none when it counted none.
    pub(super) fn answer(&self, rank: Rank, estimate: bool) -> Option<(Value, Tally)> {
        let ladder = self.ladder();
        let total = ladder.last()?.count.clone();
        if total.is_zero() {
            return None;
        }
        let at = ladder.iter().position(|step| rank.reached(&step.count, &total))?;
        let below = at.checked_sub(1).map_or_else(Tally::default, |before| ladder[before].count.clone());
        let step = &ladder[at];
        let estimate = if estimate { interpolate(step, &below, &total, rank) } else { None };
        Some((Value::Between { lower: step.lower, upper: step.upper, estimate }, total))
    }

    /// The steps a rank is found on, in the order of their bounds: the
    /// bounds every member holds, the intervals of a grid, or of both the
    /// bounds that lie on the grid.
    fn ladder(&self) -> Vec<Step> {
        let shared: Vec<(f64, &Tally)> = self
            .bounds
            .iter()
            .filter(|(_, held)| self.members > 0 && held.members == self.members)
            .map(|(bits, held)| (f64::from_bits(*bits), &held.count))
            .collect();
        match self.merged_bins() {
            None => bounds_ladder(shared.into_iter()),
            Some((_, bins)) if shared.is_empty() => bins_ladder(bins),
            Some((schema, bins)) => {
                let on_grid = |(bound, _): &(f64, &Tally)| on_grid(*bound, schema);
                let mut ladder = bounds_ladder(shared.into_iter().filter(on_grid));
                for step in &mut ladder {
                    for (_, count) in bins.iter().filter(|(bin, _)| bin.bounds().1 <= step.upper) {
                        step.count.add_tally(count);
                    }
                }
                ladder
            }
        }
    }

    /// The intervals of every grid here that counted a value, at the
    /// coarsest schema among them, in the order of their bounds, and that
    /// schema; none without any. An interval that counted nothing, as one a
    /// write began at zero, neither coarsens the others nor narrows the
    /// bounds given beside them.
    fn merged_bins(&self) -> Option<(u8, Vec<(Bin, Tally)>)> {
        let counted: Vec<(&Bin, &Tally)> = self.bins.iter().filter(|(_, count)| !count.is_zero()).collect();
        if counted.is_empty() {
            return None;
        }
        let schema = counted.iter().filter_map(|(bin, _)| bin.schema()).min().unwrap_or(FINEST);
        let mut merged: BTreeMap<Bin, Tally> = BTreeMap::new();
        for (bin, count) in counted {
            merged.entry(bin.at(schema)).or_default().add_tally(count);
        }
        Some((schema, merged.into_iter().collect()))
    }
}

/// Steps of cumulative bounds: each holds what lies above the bound before.
fn bounds_ladder<'a>(bounds: impl Iterator<Item = (f64, &'a Tally)>) -> Vec<Step> {
    let mut lower = 0.0;
    let mut ladder = Vec::new();
    for (upper, count) in bounds {
        ladder.push(Step { lower, upper, count: count.clone() });
        lower = upper;
    }
    ladder
}

/// Steps of a grid's intervals, each counting its own values: their counts
/// added up to each.
fn bins_ladder(bins: Vec<(Bin, Tally)>) -> Vec<Step> {
    let mut cumulative = Tally::default();
    let mut ladder = Vec::with_capacity(bins.len());
    for (bin, count) in bins {
        cumulative.add_tally(&count);
        let (lower, upper) = bin.bounds();
        ladder.push(Step { lower, upper, count: cumulative.clone() });
    }
    ladder
}

/// Whether a bound a program gave is one of a grid's: `+Inf` and 0 are every
/// grid's.
fn on_grid(bound: f64, schema: u8) -> bool {
    bound.is_infinite() || bound == 0.0 || Bin::of(bound, schema).bounds().1 == bound
}

/// A point of the step that holds the rank, as Prometheus'
/// `histogram_quantile` interpolates it, in floats: past the last bound the
/// last bound, and none when no bound lies below.
fn interpolate(step: &Step, below: &Tally, total: &Tally, rank: Rank) -> Option<f64> {
    if step.upper.is_infinite() {
        return (step.lower > 0.0).then_some(step.lower);
    }
    let (below, count) = (below.round(), step.count.round());
    let wanted = rank.approximately() * total.round();
    let share = if count > below { ((wanted - below) / (count - below)).clamp(0.0, 1.0) } else { 1.0 };
    Some(step.lower + (step.upper - step.lower) * share)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whole(count: u64) -> Exact {
        Exact::of_whole(count)
    }

    fn rank(percent: f64) -> Rank {
        Rank::of(percent).unwrap()
    }

    fn between(answer: Option<(Value, Tally)>) -> (f64, f64, Option<f64>) {
        match answer {
            Some((Value::Between { lower, upper, estimate }, _)) => (lower, upper, estimate),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_rank_lies_in_the_first_bound_whose_count_reaches_it() {
        let mut spread = Spread::default();
        spread.add_infinity(&whole(100));
        for (bound, count) in [(5.0, 10), (10.0, 60), (25.0, 94), (50.0, 99)] {
            spread.add_bound(bound, &whole(count));
        }
        assert_eq!(between(spread.answer(rank(50.0), true)), (5.0, 10.0, Some(9.0)));
        assert_eq!(between(spread.answer(rank(95.0), false)), (25.0, 50.0, None));
        assert_eq!(between(spread.answer(rank(10.0), false)), (0.0, 5.0, None), "the lowest begins at 0");
        assert_eq!(between(spread.answer(rank(100.0), true)), (50.0, f64::INFINITY, Some(50.0)), "past the last bound");
        assert_eq!(spread.answer(rank(50.0), false).unwrap().1, Tally::Whole(100));
    }

    #[test]
    fn bounds_a_member_does_not_hold_are_left_out_and_the_interval_widens() {
        // two members, one with [10, 20], one with [20, 40]: they share 20
        let mut spread = Spread::default();
        spread.add_infinity(&whole(10));
        spread.add_bound(10.0, &whole(4));
        spread.add_bound(20.0, &whole(8));
        spread.add_infinity(&whole(10));
        spread.add_bound(20.0, &whole(2));
        spread.add_bound(40.0, &whole(9));
        assert_eq!(between(spread.answer(rank(25.0), false)), (0.0, 20.0, None), "10 is not shared");
        assert_eq!(between(spread.answer(rank(75.0), false)), (20.0, f64::INFINITY, None), "nor is 40");
    }

    #[test]
    fn a_grids_intervals_join_at_the_coarsest_schema_and_meet_given_bounds_on_it() {
        let mut spread = Spread::default();
        let fine = |value: f64| Bin::of(value, FINEST);
        spread.add_bin(Bin::Zero, &whole(1));
        spread.add_bin(fine(1.05), &whole(2));
        spread.add_bin(fine(1.15), &whole(3));
        spread.add_bin(Bin::of(3.0, 1), &whole(4));
        // at schema 1 the first two intervals of schema 3 above 1 are one, (1, √2]
        assert_eq!(between(spread.answer(rank(10.0), false)), (0.0, 0.0, None), "the zero interval");
        let (lower, upper, _) = between(spread.answer(rank(50.0), false));
        assert_eq!((lower, upper), (1.0, Bin::of(1.1, 1).bounds().1));
        assert_eq!(between(spread.answer(rank(100.0), false)).1, Bin::of(3.0, 1).bounds().1);
        // a member with given bounds beside them: 2 lies on every grid, 3 on none
        spread.add_infinity(&whole(5));
        spread.add_bound(2.0, &whole(4));
        spread.add_bound(3.0, &whole(5));
        let counted = spread.answer(rank(50.0), false).unwrap().1;
        assert_eq!(counted, Tally::Whole(15));
        assert_eq!(between(spread.answer(rank(50.0), false)), (0.0, 2.0, None), "10 of 15 at most 2");
        assert_eq!(between(spread.answer(rank(90.0), false)), (2.0, f64::INFINITY, None));
    }

    #[test]
    fn a_spread_that_counted_nothing_has_no_answer() {
        let mut spread = Spread::default();
        assert!(spread.answer(rank(50.0), false).is_none());
        spread.add_infinity(&whole(0));
        spread.add_bound(5.0, &whole(0));
        assert!(spread.answer(rank(50.0), false).is_none());
    }
}
