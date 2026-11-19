//! How long a done job holds its id: a span from when it was done, or until
//! the local day, week or month it was done in ends, in a time zone:
//!
//! ```text
//! 10 minutes            done 23:59        held until 00:09
//! a day, Europe/Moscow  done 23:59 MSK    held until 00:00 MSK, a minute
//! ```
//!
//! A job keeps its own as text, so that its queue's may change meanwhile:
//! a span's milliseconds, `600000`, or the unit and the zone, `day
//! Europe/Moscow`.

use std::time::Duration;

use super::call::LONGEST_SPAN;
use crate::calendar::{Calendar, Zoned};
use crate::clock::millis;
use crate::{Error, Result};

/// How long a done job keeps its id taken, so that an add of it adds
/// nothing: a `Duration` from when it was done, or
/// `Dedupe::calendar(Calendar::Day, "Europe/Moscow")`, until the local day it
/// was done in ends. A day is 23 or 25 hours on the days the clock moves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dedupe(Held);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Held {
    Span(Duration),
    Calendar { unit: Calendar, zone: String },
}

impl Dedupe {
    /// Until the end of the local `unit` the job was done in, in `zone`, an
    /// IANA name, `"UTC"` among them.
    pub fn calendar(unit: Calendar, zone: &str) -> Dedupe {
        Dedupe(Held::Calendar { unit, zone: zone.to_owned() })
    }

    /// Refuses a span past the years a job may have, or a zone no one has.
    /// A queue's span of zero holds nothing, as it always did; a job's own
    /// must hold something, `zero` says.
    pub(crate) fn check(&self, zero: bool) -> Result<()> {
        match &self.0 {
            Held::Span(span) if span.is_zero() && !zero => {
                Err(Error::invalid("a dedupe of 0, which would hold nothing: leave it out"))
            }
            Held::Span(span) if millis(*span) > LONGEST_SPAN => {
                Err(Error::invalid(format!("a dedupe of {span:?}, longer than the years 1 to 9999")))
            }
            Held::Span(_) => Ok(()),
            Held::Calendar { unit, zone } => Zoned::of(*unit, zone).map(drop),
        }
    }

    /// The text a job keeps.
    pub(crate) fn text(&self) -> String {
        match &self.0 {
            Held::Span(span) => millis(*span).to_string(),
            Held::Calendar { unit, zone } => format!("{} {zone}", unit.as_str()),
        }
    }

    /// Reads a dedupe as a job keeps it.
    pub(crate) fn parse(text: &str) -> Result<Dedupe> {
        if let Ok(ms) = text.parse::<u64>() {
            return Ok(Dedupe(Held::Span(Duration::from_millis(ms))));
        }
        let units = [Calendar::Day, Calendar::Week, Calendar::Month];
        let parsed = text.split_once(' ').and_then(|(unit, zone)| {
            let unit = units.into_iter().find(|known| known.as_str() == unit)?;
            Some(Dedupe::calendar(unit, zone))
        });
        parsed.ok_or_else(|| Error::corrupt(format!("a kept dedupe {text:?}: neither a span nor a calendar")))
    }

    /// Until when a job done at `done`, unix milliseconds, holds its id.
    pub(crate) fn until(&self, done: i64) -> Result<i64> {
        match &self.0 {
            Held::Span(span) => Ok(done.saturating_add(millis(*span))),
            Held::Calendar { unit, zone } => {
                let zoned = Zoned::of(*unit, zone)?;
                Ok(zoned.end_millis(zoned.start_millis(done)))
            }
        }
    }
}

impl From<Duration> for Dedupe {
    fn from(span: Duration) -> Dedupe {
        Dedupe(Held::Span(span))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // contract: JOB-18
    #[test]
    fn a_calendar_holds_until_the_local_unit_ends_across_daylight_saving() {
        let at = |text: &str| text.parse::<jiff::Timestamp>().unwrap().as_millisecond();
        let day = Dedupe::calendar(Calendar::Day, "Europe/Moscow");
        assert_eq!(day.until(at("2026-11-19T20:59:00Z")).unwrap(), at("2026-11-19T21:00:00Z"), "23:59 MSK: a minute");
        // Berlin's clock moved back on 2026-10-25: that day is 25 hours long
        let berlin = Dedupe::calendar(Calendar::Day, "Europe/Berlin");
        assert_eq!(berlin.until(at("2026-10-24T22:30:00Z")).unwrap(), at("2026-10-25T23:00:00Z"));
        let span = Dedupe::from(Duration::from_mins(10));
        assert_eq!(span.until(at("2026-11-19T20:59:00Z")).unwrap(), at("2026-11-19T21:09:00Z"));
    }

    #[test]
    fn a_dedupe_reads_back_as_a_job_keeps_it_and_a_wrong_one_is_refused() {
        for dedupe in [Dedupe::from(Duration::from_mins(10)), Dedupe::calendar(Calendar::Week, "UTC")] {
            assert_eq!(Dedupe::parse(&dedupe.text()).unwrap(), dedupe);
        }
        assert_eq!(Dedupe::parse("fortnight UTC").unwrap_err().kind(), crate::ErrorKind::Corrupt);
        let refused = Dedupe::calendar(Calendar::Day, "Mars/Olympus").check(false).unwrap_err();
        assert!(refused.to_string().contains("no such zone"), "{refused}");
        assert!(Dedupe::from(Duration::ZERO).check(false).is_err(), "a job's own holds something");
        assert!(Dedupe::from(Duration::ZERO).check(true).is_ok(), "a queue's of zero holds nothing, as it did");
    }
}
