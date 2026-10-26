//! The range a command reads: `--since`, a span back from now, or `--from`
//! and `--to`, times in this machine's zone unless they say their own, and
//! how a person's terminal names it.
//!
//! ```text
//! --since 1h                    the last hour
//! --from 14:30 --to 14:35       today, in this machine's zone
//! --from 2026-10-25T03:00       a date and a time there
//! --from 2026-10-25T00:00:00Z   a time that says its offset
//! ```

use std::time::{Duration, SystemTime};

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

use crate::asked::{self, Asked, Refused, wrong};

/// A range: a start and an end, `None` the end being now.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Window {
    pub(crate) from: SystemTime,
    pub(crate) to: Option<SystemTime>,
    /// The span before now the range covers, when it ends now: a query
    /// asks for it so, and the page after one reads the same query.
    pub(crate) since: Option<Duration>,
}

impl Window {
    /// The range the flags give, `since` when none is given; `--since` beside
    /// `--from` or `--to` is a mistake, since each names the start.
    pub(crate) fn asked(asked: &Asked, since: Duration) -> Result<Window, Refused> {
        let now = SystemTime::now();
        let from = asked.value("--from").map(time).transpose().map_err(wrong)?;
        let to = asked.value("--to").map(time).transpose().map_err(wrong)?;
        let span = asked.value("--since").map(asked::span).transpose().map_err(wrong)?;
        match (span, from) {
            (Some(_), Some(_)) => Err(wrong("--since or --from, not both: each says where the range starts")),
            (Some(_), None) if to.is_some() => Err(wrong("--since reads up to now: --from and --to for another end")),
            (Some(span), None) => Ok(Window { from: now - span, to: None, since: Some(span) }),
            (None, Some(from)) => Ok(Window { from, to, since: None }),
            (None, None) => Ok(Window { from: to.unwrap_or(now) - since, to, since: to.is_none().then_some(since) }),
        }
    }

    /// The range as a person reads it, in this machine's zone:
    /// `26 Oct 14:32–14:33 Europe/Moscow`.
    pub(crate) fn shown(&self) -> String {
        let zone = TimeZone::system();
        let from = local(self.from, &zone);
        let to = local(self.to.unwrap_or_else(SystemTime::now), &zone);
        let named = zone.iana_name().map_or_else(|| from.strftime("%:z").to_string(), str::to_owned);
        if from.date() == to.date() {
            format!("{}–{} {named}", from.strftime("%-d %b %H:%M"), to.strftime("%H:%M"))
        } else {
            format!("{} – {} {named}", from.strftime("%-d %b %H:%M"), to.strftime("%-d %b %H:%M"))
        }
    }

    /// Whether the range lies within one day of this machine's zone, so that
    /// a time of it needs no date.
    pub(crate) fn one_day(&self) -> bool {
        let zone = TimeZone::system();
        local(self.from, &zone).date() == local(self.to.unwrap_or_else(SystemTime::now), &zone).date()
    }
}

/// A time as a person's terminal shows it: `14:32:01`, `.250` when it has
/// milliseconds, and its date before it when the range is not one day.
pub(crate) fn clock(at: SystemTime, with_date: bool) -> String {
    let at = local(at, &TimeZone::system());
    let time = if at.subsec_nanosecond() / 1_000_000 == 0 {
        at.strftime("%H:%M:%S").to_string()
    } else {
        at.strftime("%H:%M:%S%.3f").to_string()
    };
    if with_date { format!("{} {time}", at.strftime("%-d %b")) } else { time }
}

/// A span in its largest whole unit, as the flags take one: `30d`, `90m`,
/// `14s`, `250ms`.
pub(crate) fn span_text(span: Duration) -> String {
    let millis = span.as_millis();
    let units = [(86_400_000, "d"), (3_600_000, "h"), (60_000, "m"), (1000, "s")];
    match units.iter().find(|(size, _)| millis >= *size && millis.is_multiple_of(*size)) {
        Some((size, unit)) => format!("{}{unit}", millis / size),
        None if millis >= 1000 => format!("{}s", millis / 1000),
        None => format!("{millis}ms"),
    }
}

/// How long ago a time was, as a person reads it: `2s ago`, `3h ago`.
pub(crate) fn ago(at: SystemTime) -> String {
    let past = SystemTime::now().duration_since(at).unwrap_or_default().as_secs();
    let (count, unit) = match past {
        0..60 => (past, "s"),
        60..3600 => (past / 60, "m"),
        3600..86_400 => (past / 3600, "h"),
        _ => (past / 86_400, "d"),
    };
    format!("{count}{unit} ago")
}

/// A time as JSON carries it: whole, in UTC, `2026-10-26T11:32:00.250Z`.
pub(crate) fn utc(at: SystemTime) -> String {
    Timestamp::try_from(at).map_or_else(|_| String::new(), |at| at.to_string())
}

fn local(at: SystemTime, zone: &TimeZone) -> Zoned {
    Timestamp::try_from(at).unwrap_or(Timestamp::UNIX_EPOCH).to_zoned(zone.clone())
}

/// A time as `--from` and `--to` take it: one that says its offset, a date
/// and a time in this machine's zone, or a time of today there.
pub(crate) fn time(text: &str) -> Result<SystemTime, String> {
    let zone = TimeZone::system();
    let unread = || format!("the time {text}: as 14:30, 2026-10-25T03:00 or 2026-10-25T03:00:00Z");
    if let Ok(at) = text.parse::<Timestamp>() {
        return Ok(at.into());
    }
    let civil = match text.parse::<jiff::civil::DateTime>() {
        Ok(civil) => civil,
        Err(_) => {
            let time = text.parse::<jiff::civil::Time>().map_err(|_| unread())?;
            Zoned::now().with_time_zone(zone.clone()).date().to_datetime(time)
        }
    };
    let zoned = civil.to_zoned(zone).map_err(|_| unread())?;
    Ok(zoned.timestamp().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_says_its_offset_or_takes_this_machines_zone() {
        let given = time("2026-10-26T11:32:00Z").unwrap();
        assert_eq!(utc(given), "2026-10-26T11:32:00Z");
        let local = time("2026-10-26T14:32").unwrap();
        let zoned = Timestamp::try_from(local).unwrap().to_zoned(TimeZone::system());
        assert_eq!(zoned.strftime("%H:%M").to_string(), "14:32");
        assert!(time("14:30").is_ok() && time("2026-10-26 14:30").is_ok());
        assert!(time("half past two").unwrap_err().contains("as 14:30"));
    }

    #[test]
    fn a_clock_shows_milliseconds_only_when_it_has_them() {
        let whole = time("2026-10-26T11:32:00Z").unwrap();
        assert!(!clock(whole, false).contains('.'));
        assert!(clock(whole + Duration::from_millis(250), false).ends_with(".250"));
        assert!(clock(whole, true).contains("Oct"));
    }
}
