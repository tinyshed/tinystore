//! Quiet hours: two wall-clock times of day in a zone, between which a job
//! does not run. A time the store gives a job inside them becomes their end:
//!
//! ```text
//! 23:00 to 07:00 Europe/Moscow
//! a job due 22:59  → 22:59
//! a job due 23:00  → 07:00 the next morning
//! a job due 03:10  → 07:00 the same morning
//! a job due 07:00  → 07:00
//! ```
//!
//! A job keeps a window of its own as text, `23:00 07:00 Europe/Moscow`, or
//! `none` for a job no window holds, its queue's neither.

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::{AmbiguousOffset, TimeZone};

use crate::{Error, Result};

/// The text a job keeps for a call that said no quiet hours hold it.
const NONE: &str = "none";

/// Hours in which a queue's jobs, or one job, do not run: `Quiet::hours("23:00",
/// "07:00", "Europe/Moscow")`, from 23:00 to 07:00 on Moscow's clock; a `from`
/// after `to` wraps midnight. `Quiet::none()` on a call holds its job by no
/// quiet hours, its queue's neither. The times and the zone are checked where
/// the queue opens or the call is made: a time not `HH:MM`, the two the same,
/// or a zone no one has is `invalid`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quiet {
    window: Option<(String, String, String)>,
}

impl Quiet {
    pub fn hours(from: &str, to: &str, zone: &str) -> Quiet {
        Quiet { window: Some((from.to_owned(), to.to_owned(), zone.to_owned())) }
    }

    pub fn none() -> Quiet {
        Quiet { window: None }
    }

    /// The hours checked, none for `Quiet::none()`.
    pub(crate) fn checked(&self) -> Result<Option<Hours>> {
        let Some((from, to, zone)) = &self.window else {
            return Ok(None);
        };
        let described = || format!("quiet hours {from} to {to} in {zone:?}");
        let (from, to) = (minute_of(from), minute_of(to));
        let (Some(from), Some(to)) = (from, to) else {
            return Err(Error::invalid(format!("{}: a time is HH:MM, 00:00 to 23:59", described())));
        };
        if from == to {
            return Err(Error::invalid(format!("{}: from and to the same, which holds nothing", described())));
        }
        let found = TimeZone::get(zone).map_err(|_| Error::invalid(format!("time zone {zone:?}: no such zone")))?;
        Ok(Some(Hours { from, to, zone: found, name: zone.clone() }))
    }

    /// The quiet a job kept, as a write passed on carries it.
    #[cfg(feature = "replication")]
    pub(crate) fn kept_as(text: &str) -> Result<Quiet> {
        if text == NONE {
            return Ok(Quiet::none());
        }
        let mut parts = text.splitn(3, ' ');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(from), Some(to), Some(zone)) => Ok(Quiet::hours(from, to, zone)),
            _ => Err(Error::corrupt(format!("a job's quiet hours {text:?}: not from, to and a zone"))),
        }
    }

    /// What a job keeps of a call's quiet: its window's text, or `none`.
    pub(crate) fn kept(&self) -> Result<String> {
        Ok(self.checked()?.map_or_else(|| NONE.to_owned(), |hours| hours.text()))
    }
}

/// Quiet hours checked: minutes of the day from midnight, and the zone.
#[derive(Clone, Debug)]
pub(crate) struct Hours {
    from: u16,
    to: u16,
    zone: TimeZone,
    name: String,
}

impl PartialEq for Hours {
    fn eq(&self, other: &Hours) -> bool {
        (self.from, self.to, &self.name) == (other.from, other.to, &other.name)
    }
}

impl Eq for Hours {}

impl Hours {
    /// Reads what a job keeps; none for `none`, its window turned off.
    pub(crate) fn parse(text: &str) -> Result<Option<Hours>> {
        if text == NONE {
            return Ok(None);
        }
        let mut parts = text.splitn(3, ' ');
        let (Some(from), Some(to), Some(zone)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(Error::corrupt(format!("a job's quiet hours {text:?}: not from, to and a zone")));
        };
        Quiet::hours(from, to, zone)
            .checked()
            .map_err(|error| Error::corrupt(format!("a job's quiet hours {text:?}: {error}")))
    }

    pub(crate) fn text(&self) -> String {
        let at = |minute: u16| format!("{:02}:{:02}", minute / 60, minute % 60);
        format!("{} {} {}", at(self.from), at(self.to), self.name)
    }

    /// `at`, unix milliseconds, or the end of these hours when it falls
    /// inside them. An end that daylight saving skips is the skip's end, as
    /// a cron's time is; one it repeats is the first of the two after `at`.
    pub(crate) fn hold(&self, at: i64) -> i64 {
        let Ok(instant) = Timestamp::from_millisecond(at) else {
            return at;
        };
        let wall = instant.to_zoned(self.zone.clone()).datetime();
        let minute = u16::try_from(i32::from(wall.hour()) * 60 + i32::from(wall.minute())).unwrap_or(0);
        let (inside, ends_tomorrow) = if self.from < self.to {
            ((self.from..self.to).contains(&minute), false)
        } else {
            (minute >= self.from || minute < self.to, minute >= self.from)
        };
        if !inside {
            return at;
        }
        let day = if ends_tomorrow { wall.date().tomorrow().ok() } else { Some(wall.date()) };
        self.end_on(day, at).unwrap_or(at)
    }

    /// The end of the hours on `day` after `at`: on the night the clock goes
    /// back and shows the end twice, the second when `at` lies past the first,
    /// as 02:15 after the fold does in hours of 02:00 to 02:30.
    fn end_on(&self, day: Option<Date>, at: i64) -> Option<i64> {
        let (hour, minute) = (i8::try_from(self.to / 60).ok()?, i8::try_from(self.to % 60).ok()?);
        let wall = day?.at(hour, minute, 0, 0);
        let first = super::cron::instant(&self.zone, wall)?.as_millisecond();
        if first > at {
            return Some(first);
        }
        match self.zone.to_ambiguous_zoned(wall).offset() {
            AmbiguousOffset::Fold { after, .. } => {
                after.to_timestamp(wall).ok().map(|end| end.as_millisecond()).filter(|end| *end > at)
            }
            _ => None,
        }
    }
}

/// The minute of the day `HH:MM` names, two digits each.
fn minute_of(text: &str) -> Option<u16> {
    let (hour, minute) = text.split_once(':')?;
    let digits = |part: &str| part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(hour) || !digits(minute) {
        return None;
    }
    let (hour, minute) = (hour.parse::<u16>().ok()?, minute.parse::<u16>().ok()?);
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    fn at(text: &str) -> i64 {
        text.parse::<jiff::Zoned>().unwrap().timestamp().as_millisecond()
    }

    fn night() -> Hours {
        Quiet::hours("23:00", "07:00", "Europe/Moscow").checked().unwrap().unwrap()
    }

    #[test]
    fn a_time_inside_the_hours_is_their_end_and_one_outside_is_itself() {
        let night = night();
        let moscow = |time: &str| at(&format!("{time}[Europe/Moscow]"));
        for (due, held) in [
            ("2026-11-26T22:59:59", "2026-11-26T22:59:59"),
            ("2026-11-26T23:00:00", "2026-11-27T07:00:00"),
            ("2026-11-27T03:10:00", "2026-11-27T07:00:00"),
            ("2026-11-27T06:59:59", "2026-11-27T07:00:00"),
            ("2026-11-27T07:00:00", "2026-11-27T07:00:00"),
            ("2026-11-27T12:00:00", "2026-11-27T12:00:00"),
        ] {
            assert_eq!(night.hold(moscow(due)), moscow(held), "{due}");
        }
        let lunch = Quiet::hours("12:00", "13:30", "UTC").checked().unwrap().unwrap();
        assert_eq!(lunch.hold(at("2026-11-27T12:45:00[UTC]")), at("2026-11-27T13:30:00[UTC]"));
        assert_eq!(lunch.hold(at("2026-11-27T13:30:00[UTC]")), at("2026-11-27T13:30:00[UTC]"));
    }

    #[test]
    fn an_end_daylight_saving_skips_is_the_skips_end_and_hours_follow_the_wall_clock() {
        // Berlin's clock went from 02:00 to 03:00 on 29 March 2026
        let night = Quiet::hours("22:00", "02:30", "Europe/Berlin").checked().unwrap().unwrap();
        let held = night.hold(at("2026-03-28T23:00:00[Europe/Berlin]"));
        assert_eq!(held, at("2026-03-29T03:00:00[Europe/Berlin]"));
        // and back from 03:00 to 02:00 on 25 October: the wall clock's 07:00 still ends the night
        let night = Quiet::hours("23:00", "07:00", "Europe/Berlin").checked().unwrap().unwrap();
        assert_eq!(night.hold(at("2026-10-24T23:30:00[Europe/Berlin]")), at("2026-10-25T07:00:00[Europe/Berlin]"));
    }

    #[test]
    fn hours_the_clock_going_back_shows_twice_end_at_the_end_after_the_time() {
        // Berlin's clock went back from 03:00 to 02:00 on 25 October 2026
        let early = Quiet::hours("02:00", "02:30", "Europe/Berlin").checked().unwrap().unwrap();
        let (before, after) = ("2026-10-25T02:15:00+02:00[Europe/Berlin]", "2026-10-25T02:15:00+01:00[Europe/Berlin]");
        assert_eq!(early.hold(at(before)), at("2026-10-25T02:30:00+02:00[Europe/Berlin]"));
        assert_eq!(early.hold(at(after)), at("2026-10-25T02:30:00+01:00[Europe/Berlin]"));
    }

    #[test]
    fn a_window_kept_reads_back_and_one_that_does_not_check_is_refused() {
        assert_eq!(Hours::parse(&night().text()).unwrap(), Some(night()));
        assert_eq!(Hours::parse(&Quiet::none().kept().unwrap()).unwrap(), None);
        for (from, to, zone) in [("23:00", "23:00", "UTC"), ("7:00", "08:00", "UTC"), ("24:00", "01:00", "UTC")] {
            assert_eq!(Quiet::hours(from, to, zone).checked().unwrap_err().kind(), ErrorKind::Invalid, "{from}");
        }
        assert_eq!(Quiet::hours("23:00", "07:00", "Mars/Olympus").checked().unwrap_err().kind(), ErrorKind::Invalid);
    }
}
