//! Five-field cron expressions on a zone's wall clock.

use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use jiff::tz::{AmbiguousOffset, TimeZone};

/// How far ahead a search for the next time looks: an expression that runs at
/// all runs within it, the 29th of February among them.
const YEARS: i16 = 5;

/// A parsed expression: a bit for each value a field accepts, and whether the
/// days of the month and of the week were each left as `*`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cron {
    /// The fields as given, one space apart, which is how a job keeps them.
    pub(crate) spelling: String,
    minutes: u64,
    hours: u64,
    days: u64,
    months: u64,
    weekdays: u64,
    any_day: bool,
    any_weekday: bool,
}

const SHORTHANDS: [(&str, &str); 7] = [
    ("@yearly", "0 0 1 1 *"),
    ("@annually", "0 0 1 1 *"),
    ("@monthly", "0 0 1 * *"),
    ("@weekly", "0 0 * * 0"),
    ("@daily", "0 0 * * *"),
    ("@midnight", "0 0 * * *"),
    ("@hourly", "0 * * * *"),
];

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// A field's range and the names its values may go by.
struct Field {
    low: u32,
    high: u32,
    names: &'static [&'static str],
}

const FIELDS: [Field; 5] = [
    Field { low: 0, high: 59, names: &[] },
    Field { low: 0, high: 23, names: &[] },
    Field { low: 1, high: 31, names: &[] },
    Field { low: 1, high: 12, names: &MONTHS },
    Field { low: 0, high: 7, names: &WEEKDAYS },
];

impl Cron {
    /// Reads five fields — minute, hour, day of the month, month, day of the
    /// week — each `*`, a value, a range or a list, with an optional step, or a
    /// shorthand such as `@daily`. The error says what does not read.
    pub(crate) fn parse(expr: &str) -> Result<Cron, String> {
        let trimmed = expr.trim();
        let full = SHORTHANDS
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(trimmed))
            .map_or(trimmed, |(_, fields)| *fields);
        let fields: Vec<&str> = full.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!("{} fields, not five", fields.len()));
        }
        let mut sets = [0u64; 5];
        for ((set, field), text) in sets.iter_mut().zip(&FIELDS).zip(&fields) {
            *set = parse_field(text, field)?;
        }
        // 7 is Sunday, as 0 is
        let weekdays = if sets[4] & (1 << 7) != 0 { (sets[4] | 1) & 0x7f } else { sets[4] };
        Ok(Cron {
            spelling: fields.join(" "),
            minutes: sets[0],
            hours: sets[1],
            days: sets[2],
            months: sets[3],
            weekdays,
            any_day: fields[2] == "*",
            any_weekday: fields[4] == "*",
        })
    }

    /// The first time after `after` whose wall clock in `zone` the fields
    /// accept. A wall time daylight saving skips runs when the skip ends; one it
    /// repeats runs once, at its first instant.
    pub(crate) fn next(&self, zone: &TimeZone, after: Timestamp) -> Option<Timestamp> {
        let wall = after.to_zoned(zone.clone()).datetime();
        let (mut year, mut month, mut day) = (wall.year(), wall.month(), wall.day());
        let (mut hour, mut minute) = (wall.hour(), wall.minute() + 1);
        while year <= wall.year() + YEARS {
            while month <= 12 {
                if self.months & (1 << month) != 0 {
                    let last = Date::new(year, month, 1).ok()?.days_in_month();
                    while day <= last {
                        if self.day_matches(year, month, day)
                            && let Some(found) = self.first_in_day(zone, (year, month, day), hour, minute, after)
                        {
                            return Some(found);
                        }
                        (day, hour, minute) = (day + 1, 0, 0);
                    }
                }
                (month, day, hour, minute) = (month + 1, 1, 0, 0);
            }
            (year, month, day, hour, minute) = (year + 1, 1, 1, 0, 0);
        }
        None
    }

    /// The first accepted time of one day from `hour` and `minute` on.
    fn first_in_day(
        &self,
        zone: &TimeZone,
        (year, month, day): (i16, i8, i8),
        mut hour: i8,
        mut minute: i8,
        after: Timestamp,
    ) -> Option<Timestamp> {
        while hour < 24 {
            if self.hours & (1 << hour) != 0 {
                while minute < 60 {
                    let rest = self.minutes & (u64::MAX << minute);
                    if rest == 0 {
                        break;
                    }
                    minute = i8::try_from(rest.trailing_zeros()).ok()?;
                    let found = instant(zone, DateTime::new(year, month, day, hour, minute, 0, 0).ok()?)?;
                    if found > after {
                        return Some(found);
                    }
                    minute += 1;
                }
            }
            (hour, minute) = (hour + 1, 0);
        }
        None
    }

    /// A day is taken by its day of the month or of the week, by either when
    /// both are given, as cron always has.
    fn day_matches(&self, year: i16, month: i8, day: i8) -> bool {
        let Ok(date) = Date::new(year, month, day) else {
            return false;
        };
        let by_day = self.days & (1 << day) != 0;
        let by_weekday = self.weekdays & (1 << date.weekday().to_sunday_zero_offset()) != 0;
        match (self.any_day, self.any_weekday) {
            (true, true) => true,
            (true, false) => by_weekday,
            (false, true) => by_day,
            (false, false) => by_day || by_weekday,
        }
    }
}

/// A wall time of `zone` as an instant: the end of the skip for one daylight
/// saving skips, the first of the two for one it repeats.
fn instant(zone: &TimeZone, wall: DateTime) -> Option<Timestamp> {
    match zone.to_ambiguous_zoned(wall).offset() {
        AmbiguousOffset::Unambiguous { offset } => offset.to_timestamp(wall).ok(),
        AmbiguousOffset::Fold { before, .. } => before.to_timestamp(wall).ok(),
        AmbiguousOffset::Gap { after, .. } => {
            // read with the offset after the skip, the wall time lies before the
            // skip, and the skip is the next transition from there
            let before_skip = after.to_timestamp(wall).ok()?;
            zone.following(before_skip).next().map(|transition| transition.timestamp())
        }
    }
}

/// A list of items, each `*`, a value or a range, with an optional step:
/// `*/15`, `1-5`, `9-18/3`, `mon-fri`.
fn parse_field(text: &str, field: &Field) -> Result<u64, String> {
    let mut set = 0u64;
    for item in text.split(',') {
        let (span, step) = match item.split_once('/') {
            Some((span, step)) => match step.parse::<u32>() {
                Ok(step) if step >= 1 => (span, Some(step)),
                _ => return Err(format!("a step of {item:?}")),
            },
            None => (item, None),
        };
        let (first, mut last) = parse_span(span, field)?;
        if step.is_some() && first == last && span != "*" {
            last = field.high;
        }
        let mut value = first;
        while value <= last {
            set |= 1 << value;
            value += step.unwrap_or(1);
        }
    }
    Ok(set)
}

fn parse_span(span: &str, field: &Field) -> Result<(u32, u32), String> {
    if span == "*" {
        return Ok((field.low, field.high));
    }
    let (first, last) = match span.split_once('-') {
        Some((from, to)) => (parse_value(from, field)?, parse_value(to, field)?),
        None => {
            let value = parse_value(span, field)?;
            (value, value)
        }
    };
    if last < first {
        return Err(format!("a range of {span:?}"));
    }
    Ok((first, last))
}

fn parse_value(text: &str, field: &Field) -> Result<u32, String> {
    if let Some(at) = field.names.iter().position(|name| name.eq_ignore_ascii_case(text)) {
        return u32::try_from(at).map(|at| at + field.low).map_err(|_| format!("a value of {text:?}"));
    }
    match text.parse::<u32>() {
        Ok(value) if (field.low..=field.high).contains(&value) => Ok(value),
        _ => Err(format!("a value of {text:?}, not {} to {}", field.low, field.high)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn next(expr: &str, zone: &str, after: &str) -> String {
        let cron = Cron::parse(expr).unwrap();
        cron.next(&TimeZone::get(zone).unwrap(), at(after)).unwrap().to_string()
    }

    #[test]
    fn a_cron_runs_at_the_next_minute_its_fields_accept() {
        assert_eq!(next("10 3 * * *", "UTC", "2026-10-09T12:00:00Z"), "2026-10-10T03:10:00Z");
        assert_eq!(next("*/15 9-18 * * 1-5", "UTC", "2026-10-09T09:01:00Z"), "2026-10-09T09:15:00Z");
        assert_eq!(next("0 9 * * mon", "UTC", "2026-10-09T12:00:00Z"), "2026-10-12T09:00:00Z");
        assert_eq!(next("@hourly", "UTC", "2026-10-09T12:00:00Z"), "2026-10-09T13:00:00Z");
        assert_eq!(next("0 0 29 2 *", "UTC", "2026-10-09T12:00:00Z"), "2028-02-29T00:00:00Z");
    }

    #[test]
    fn a_cron_reads_the_wall_clock_of_its_zone() {
        // Berlin is two hours ahead of UTC in summer
        assert_eq!(next("0 20 * * *", "Europe/Berlin", "2026-07-01T12:00:00Z"), "2026-07-01T18:00:00Z");
    }

    #[test]
    fn a_time_daylight_saving_skips_runs_when_the_skip_ends() {
        // Berlin's clocks went from 02:00 to 03:00 on 29 March 2026
        assert_eq!(next("30 2 * * *", "Europe/Berlin", "2026-03-28T12:00:00Z"), "2026-03-29T01:00:00Z");
    }

    #[test]
    fn a_time_daylight_saving_repeats_runs_once() {
        // Berlin's clocks went from 03:00 back to 02:00 on 25 October 2026
        let first = next("30 2 * * *", "Europe/Berlin", "2026-10-24T12:00:00Z");
        assert_eq!(first, "2026-10-25T00:30:00Z");
        assert_eq!(next("30 2 * * *", "Europe/Berlin", &first), "2026-10-26T01:30:00Z");
    }

    #[test]
    fn a_day_of_the_month_and_of_the_week_both_given_run_on_either() {
        // the 13th, or a Friday
        assert_eq!(next("0 0 13 * fri", "UTC", "2026-10-09T12:00:00Z"), "2026-10-13T00:00:00Z");
        assert_eq!(next("0 0 13 * fri", "UTC", "2026-10-13T12:00:00Z"), "2026-10-16T00:00:00Z");
    }

    #[test]
    fn what_is_not_cron_says_why() {
        assert_eq!(Cron::parse("* * * *").unwrap_err(), "4 fields, not five");
        assert!(Cron::parse("61 * * * *").unwrap_err().contains("\"61\""));
        assert!(Cron::parse("*/0 * * * *").unwrap_err().contains("step"));
        assert!(Cron::parse("5-1 * * * *").unwrap_err().contains("range"));
        assert_eq!(Cron::parse("0  9 * *   mon").unwrap().spelling, "0 9 * * mon");
    }

    #[test]
    fn a_cron_that_never_runs_has_no_next() {
        let never = Cron::parse("0 0 31 2 *").unwrap();
        assert_eq!(never.next(&TimeZone::UTC, at("2026-01-01T00:00:00Z")), None);
    }
}
