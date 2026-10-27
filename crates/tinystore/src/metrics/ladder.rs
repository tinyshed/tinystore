//! Where packing cuts blocks: where a unit of this ladder begins, counted from
//! 1970 in UTC. Each unit divides a day, so a bucket of whole units holds
//! whole blocks and an aggregate answers it from their summaries; the Go
//! engine cut 240 samples wherever they began, and an hourly bucket cut
//! nearly every block.
//!
//! A series takes the largest unit that holds at most 320 of its samples:
//!
//! ```text
//! a sample every   15 s    1 min    10 s    5 min    1 s      100 ms   1 ms
//! a block          1 h     4 h      30 min  1 d      5 min    30 s     200 ms
//! ```
//!
//! The ladder reaches below a minute for a series faster than four samples a
//! second: were a minute its shortest unit, a head would wait for one to end
//! with more samples than it may hold.

use super::codec::MOST_SAMPLES;

const SECOND: i64 = 1000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;
pub(super) const DAY: i64 = 24 * HOUR;

/// The units, the shortest first. The shortest holds 200 samples a
/// millisecond apart, the closest a series' times lie.
const UNITS: [i64; 22] = [
    200,
    500,
    SECOND,
    2 * SECOND,
    5 * SECOND,
    10 * SECOND,
    15 * SECOND,
    20 * SECOND,
    30 * SECOND,
    MINUTE,
    5 * MINUTE,
    10 * MINUTE,
    15 * MINUTE,
    30 * MINUTE,
    HOUR,
    2 * HOUR,
    3 * HOUR,
    4 * HOUR,
    6 * HOUR,
    8 * HOUR,
    12 * HOUR,
    DAY,
];

/// The unit of `count` samples from `start` to `end`: the largest whose span
/// holds at most 320 of them at their mean step. A unit that holds more all
/// the same, where a series wrote faster for a while, packing cuts every 320
/// samples.
pub(super) fn unit_of(count: usize, start: i64, end: i64) -> i64 {
    let Some(gaps) = count.checked_sub(1).filter(|gaps| *gaps > 0) else { return DAY };
    let step = end.saturating_sub(start) / gaps as i64;
    let holds = step.saturating_mul(MOST_SAMPLES as i64);
    UNITS.iter().rev().copied().find(|unit| *unit <= holds).unwrap_or(UNITS[0])
}

/// Where the unit holding `at` begins.
pub(super) fn unit_start(at: i64, unit: i64) -> i64 {
    at.div_euclid(unit) * unit
}

/// Where the unit holding `at` ends, and the next begins.
pub(super) fn unit_end(at: i64, unit: i64) -> i64 {
    unit_start(at, unit).saturating_add(unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_series_takes_the_largest_unit_that_holds_320_of_its_samples() {
        let unit = |step: i64| unit_of(1000, 0, 999 * step);
        assert_eq!(unit(15_000), HOUR);
        assert_eq!(unit(60_000), 4 * HOUR);
        assert_eq!(unit(10_000), 30 * MINUTE);
        assert_eq!(unit(5 * MINUTE), DAY);
        assert_eq!(unit(HOUR), DAY);
        assert_eq!(unit(1000), 5 * MINUTE);
        assert_eq!(unit(999), 5 * MINUTE, "a sample a second, a little early, still fills five minutes");
        assert_eq!(unit(100), 30 * SECOND);
        assert_eq!(unit(1), 200, "a sample a millisecond, the closest times lie");
        assert_eq!(unit(11_240), 30 * MINUTE, "a step a little short of 11.25 s holds more than 320 in an hour");
        assert_eq!(unit_of(1, 5, 5), DAY);
        assert_eq!(unit_of(2, 5, 5), 200);
    }

    #[test]
    fn every_unit_divides_a_day_and_none_holds_more_than_a_head_at_a_millisecond_a_sample() {
        assert!(UNITS.iter().all(|unit| DAY % unit == 0), "a bucket of whole units holds whole blocks");
        assert!(UNITS.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(UNITS[0] <= MOST_SAMPLES as i64);
    }

    #[test]
    fn a_unit_is_counted_from_1970_in_utc_on_either_side_of_it() {
        assert_eq!(unit_start(3 * HOUR + 5, HOUR), 3 * HOUR);
        assert_eq!(unit_end(3 * HOUR, HOUR), 4 * HOUR);
        assert_eq!(unit_start(-1, HOUR), -HOUR);
        assert_eq!(unit_end(i64::MAX - 5, DAY), i64::MAX);
    }
}
