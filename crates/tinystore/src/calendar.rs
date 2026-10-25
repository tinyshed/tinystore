//! Buckets on the calendar of a time zone, which an aggregate of metrics or
//! of records asks for by a unit and a zone: a shop's "orders a day" counted
//! in the shop's own days.
//!
//! A bucket begins at the start of a local day, of a week from its Monday or
//! of a month, as an instant, and ends where the next begins. A day is 23
//! or 25 hours on the days the clock moves, and a month is as long as it is:
//!
//! ```text
//! Europe/Berlin, a day    2026-03-29  from 2026-03-28T23:00Z to 2026-03-29T22:00Z, 23 hours
//! Asia/Kolkata, a day     2026-10-21  from 2026-10-20T18:30Z to 2026-10-21T18:30Z
//! ```

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};

use crate::{Error, Result};

/// The span of the calendar a bucket covers, in the time zone an aggregate
/// gives with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Calendar {
    /// From a local midnight to the next.
    Day,
    /// From a Monday's local midnight to the next Monday's, as ISO 8601 counts weeks.
    Week,
    /// From the first of a month's local midnight to the next month's.
    Month,
}

impl Calendar {
    /// The unit as the wire and every SDK spell it: `day`, `week`, `month`.
    pub fn as_str(self) -> &'static str {
        match self {
            Calendar::Day => "day",
            Calendar::Week => "week",
            Calendar::Month => "month",
        }
    }

    /// The unit and the zone an aggregate's message names, both or neither.
    #[cfg(feature = "wire")]
    pub(crate) fn asked<'z>(unit: Option<&str>, zone: Option<&'z str>) -> Result<Option<(Calendar, &'z str)>> {
        let of = |word: &str| {
            let units = [Calendar::Day, Calendar::Week, Calendar::Month];
            let unit = units.into_iter().find(|unit| unit.as_str() == word);
            unit.ok_or_else(|| Error::invalid(format!("a calendar {word:?}: day, week or month")))
        };
        match (unit, zone) {
            (Some(unit), Some(zone)) => Ok(Some((of(unit)?, zone))),
            (None, None) => Ok(None),
            _ => Err(Error::invalid("a calendar takes its time zone, and a time zone a calendar")),
        }
    }
}

/// Buckets of one unit in one zone.
#[derive(Clone, Debug)]
pub(crate) struct Zoned {
    unit: Calendar,
    zone: TimeZone,
}

impl Zoned {
    /// The buckets of `unit` in the zone of IANA name `zone`, `UTC` among them.
    pub(crate) fn of(unit: Calendar, zone: &str) -> Result<Zoned> {
        let zone = TimeZone::get(zone).map_err(|_| Error::invalid(format!("time zone {zone:?}: no such zone")))?;
        Ok(Zoned { unit, zone })
    }

    /// Where the bucket that holds `at` begins, both in nanoseconds since
    /// 1970; `at` itself for a time the calendar cannot reach.
    #[cfg(feature = "records")]
    pub(crate) fn start_nanos(&self, at: i64) -> i64 {
        let start = Timestamp::from_nanosecond(i128::from(at)).ok().and_then(|at| self.start(at));
        start.and_then(|start| i64::try_from(start.as_nanosecond()).ok()).unwrap_or(at)
    }

    /// Where the bucket that holds `at` begins, both in milliseconds since
    /// 1970; `at` itself for a time the calendar cannot reach.
    #[cfg(feature = "metrics")]
    pub(crate) fn start_millis(&self, at: i64) -> i64 {
        let start = Timestamp::from_millisecond(at).ok().and_then(|at| self.start(at));
        start.map_or(at, |start| start.as_millisecond())
    }

    /// Where the bucket that begins at `start` ends, the next one's start,
    /// in milliseconds since 1970; the end of time past what it can reach.
    #[cfg(feature = "metrics")]
    pub(crate) fn end_millis(&self, start: i64) -> i64 {
        let end = Timestamp::from_millisecond(start).ok().and_then(|start| self.end(start));
        end.map_or(i64::MAX, |end| end.as_millisecond())
    }

    fn start(&self, at: Timestamp) -> Option<Timestamp> {
        let day = at.to_zoned(self.zone.clone()).date();
        let first = match self.unit {
            Calendar::Day => day,
            Calendar::Week => day.checked_sub(Span::new().days(day.weekday().to_monday_zero_offset())).ok()?,
            Calendar::Month => day.first_of_month(),
        };
        self.midnight(first)
    }

    #[cfg(feature = "metrics")]
    fn end(&self, start: Timestamp) -> Option<Timestamp> {
        let day = start.to_zoned(self.zone.clone()).date();
        let next = match self.unit {
            Calendar::Day => day.tomorrow().ok()?,
            Calendar::Week => day.checked_add(Span::new().days(7)).ok()?,
            Calendar::Month => day.first_of_month().checked_add(Span::new().months(1)).ok()?,
        };
        self.midnight(next)
    }

    /// The instant a local day begins: its midnight, or the first time it
    /// has where the clock skipped midnight.
    fn midnight(&self, day: Date) -> Option<Timestamp> {
        Some(day.to_zoned(self.zone.clone()).ok()?.start_of_day().ok()?.timestamp())
    }
}

#[cfg(all(test, feature = "metrics"))]
mod tests {
    use super::*;

    /// The UTC instants of the buckets from the one that holds `from`, as
    /// RFC 3339 text, `count` of them.
    fn buckets(unit: Calendar, zone: &str, from: &str, count: usize) -> Vec<String> {
        let zoned = Zoned::of(unit, zone).unwrap();
        let mut at = zoned.start_millis(from.parse::<Timestamp>().unwrap().as_millisecond());
        let mut found = Vec::new();
        for _ in 0..count {
            found.push(Timestamp::from_millisecond(at).unwrap().to_string());
            at = zoned.end_millis(at);
        }
        found
    }

    #[test]
    fn a_day_begins_at_local_midnight_and_lasts_23_or_25_hours_the_days_the_clock_moves() {
        let spring = buckets(Calendar::Day, "Europe/Berlin", "2026-03-28T12:00:00Z", 3);
        assert_eq!(spring, ["2026-03-27T23:00:00Z", "2026-03-28T23:00:00Z", "2026-03-29T22:00:00Z"]);
        let autumn = buckets(Calendar::Day, "Europe/Berlin", "2026-10-24T12:00:00Z", 3);
        assert_eq!(autumn, ["2026-10-23T22:00:00Z", "2026-10-24T22:00:00Z", "2026-10-25T23:00:00Z"]);
        let moscow = buckets(Calendar::Day, "Europe/Moscow", "2026-10-21T22:30:00Z", 2);
        assert_eq!(moscow, ["2026-10-21T21:00:00Z", "2026-10-22T21:00:00Z"]);
    }

    #[test]
    fn a_zone_a_half_or_three_quarters_of_an_hour_off_utc_begins_its_day_there() {
        assert_eq!(buckets(Calendar::Day, "Asia/Kolkata", "2026-10-21T12:00:00Z", 1), ["2026-10-20T18:30:00Z"]);
        assert_eq!(buckets(Calendar::Day, "Asia/Kathmandu", "2026-10-21T12:00:00Z", 1), ["2026-10-20T18:15:00Z"]);
    }

    #[test]
    fn a_week_begins_on_monday_across_a_years_end_and_a_month_is_as_long_as_it_is() {
        // 2026-01-01 is a Thursday: its week began on Monday 2025-12-29
        let week = buckets(Calendar::Week, "UTC", "2026-01-01T12:00:00Z", 2);
        assert_eq!(week, ["2025-12-29T00:00:00Z", "2026-01-05T00:00:00Z"]);
        let months = buckets(Calendar::Month, "Europe/Berlin", "2026-02-10T00:00:00Z", 3);
        assert_eq!(months, ["2026-01-31T23:00:00Z", "2026-02-28T23:00:00Z", "2026-03-31T22:00:00Z"]);
    }

    #[test]
    fn a_day_whose_midnight_the_clock_skips_begins_at_its_first_hour() {
        // Santiago moved its clock from 00:00 to 01:00 on 2026-09-06: that day begins at 01:00, 23 hours long
        let skipped = buckets(Calendar::Day, "America/Santiago", "2026-09-05T12:00:00Z", 3);
        assert_eq!(skipped, ["2026-09-05T04:00:00Z", "2026-09-06T04:00:00Z", "2026-09-07T03:00:00Z"]);
    }

    #[test]
    fn a_zone_no_one_has_is_refused() {
        let error = Zoned::of(Calendar::Day, "Mars/Olympus").unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Invalid, "{error}");
    }
}
