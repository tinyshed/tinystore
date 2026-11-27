//! The range a command reads: `--since`, a span back from now, or `--from`
//! and `--to`, times in the command's zone unless they say their own, and
//! how a person's terminal names it.
//!
//! ```text
//! --since 1h                    the last hour
//! --from 14:30 --to 14:35       today, in the command's zone
//! --from 2026-10-25T03:00       a date and a time there
//! --from 2026-10-25T00:00:00Z   a time that says its offset
//! ```
//!
//! The command's zone is `--tz`'s, else `TINYSTORE_TZ`'s, else this
//! machine's: every time printed for a person and every time read without
//! its offset is in it, and JSON is UTC whatever it is.

use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

use crate::asked::{self, Asked, Refused, wrong};

/// The zone a command prints and reads times in, chosen once before it runs.
static ZONE: OnceLock<TimeZone> = OnceLock::new();

/// The command's zone: this machine's until `choose_zone` chose another.
pub(crate) fn zone() -> &'static TimeZone {
    ZONE.get_or_init(TimeZone::system)
}

/// Chooses the command's zone: `--tz`'s when given, else `TINYSTORE_TZ`'s
/// when set, else this machine's. A zone no one knows is refused, named
/// with the nearest one.
pub(crate) fn choose_zone(flag: Option<&str>) -> Result<(), String> {
    let from_env = || std::env::var("TINYSTORE_TZ").ok().filter(|name| !name.is_empty());
    let given = match flag {
        Some(name) => ("--tz", name.to_owned()),
        None => match from_env() {
            Some(name) => ("TINYSTORE_TZ", name),
            None => return Ok(()),
        },
    };
    let (source, name) = given;
    let chosen = zone_named(&name).map_err(|why| format!("{source} {name}: {why}"))?;
    // a zone chosen twice in one process keeps the first, which a test alone does
    let _ = ZONE.set(chosen);
    Ok(())
}

/// A zone by its IANA name, `utc` or `local`, this machine's.
fn zone_named(name: &str) -> Result<TimeZone, String> {
    if name.eq_ignore_ascii_case("utc") {
        return Ok(TimeZone::UTC);
    }
    if name.eq_ignore_ascii_case("local") {
        return Ok(TimeZone::system());
    }
    TimeZone::get(name).map_err(|_| {
        let known: Vec<String> = jiff::tz::db().available().map(|known| known.as_str().to_owned()).collect();
        let names: Vec<&str> = known.iter().map(String::as_str).chain(["utc", "local"]).collect();
        format!("no such zone: an IANA name as Asia/Tokyo, utc or local{}", crate::meant(name, &names))
    })
}

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

    /// The range as a person reads it, in the command's zone:
    /// `26 Oct 14:32–14:33 Europe/Moscow`.
    pub(crate) fn shown(&self) -> String {
        let zone = zone();
        let from = local(self.from, zone);
        let to = local(self.to.unwrap_or_else(SystemTime::now), zone);
        let named = zone.iana_name().map_or_else(|| from.strftime("%:z").to_string(), str::to_owned);
        if from.date() == to.date() {
            format!("{}–{} {named}", from.strftime("%-d %b %H:%M"), to.strftime("%H:%M"))
        } else {
            format!("{} – {} {named}", from.strftime("%-d %b %H:%M"), to.strftime("%-d %b %H:%M"))
        }
    }

    /// Whether the range lies within one day of the command's zone, so that
    /// a time of it needs no date.
    pub(crate) fn one_day(&self) -> bool {
        local(self.from, zone()).date() == local(self.to.unwrap_or_else(SystemTime::now), zone()).date()
    }
}

/// A time as a person's terminal shows it: `14:32:01`, `.250` when it has
/// milliseconds, and its date before it when the range is not one day.
pub(crate) fn clock(at: SystemTime, with_date: bool) -> String {
    let at = local(at, zone());
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

/// A time's date in the command's zone: `2026-10-31`.
pub(crate) fn date(at: SystemTime) -> String {
    local(at, zone()).strftime("%Y-%m-%d").to_string()
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

/// A time in a zone, as a person's terminal shows one.
pub(crate) fn local(at: SystemTime, zone: &TimeZone) -> Zoned {
    Timestamp::try_from(at).unwrap_or(Timestamp::UNIX_EPOCH).to_zoned(zone.clone())
}

/// A time as `--from` and `--to` take it: one that says its offset, a date
/// and a time in the command's zone, or a time of today there.
pub(crate) fn time(text: &str) -> Result<SystemTime, String> {
    time_in(text, zone())
}

fn time_in(text: &str, zone: &TimeZone) -> Result<SystemTime, String> {
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
    let zoned = civil.to_zoned(zone.clone()).map_err(|_| unread())?;
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

    #[test]
    fn a_time_without_its_offset_is_read_in_the_zone_given() {
        let tokyo = zone_named("Asia/Tokyo").unwrap();
        assert_eq!(utc(time_in("2026-10-26T14:32", &tokyo).unwrap()), "2026-10-26T05:32:00Z");
        assert_eq!(utc(time_in("2026-10-26T14:32", &zone_named("UTC").unwrap()).unwrap()), "2026-10-26T14:32:00Z");
        assert_eq!(utc(time_in("2026-10-26T14:32:00Z", &tokyo).unwrap()), "2026-10-26T14:32:00Z", "its own offset");
        let shown = local(time_in("2026-10-26T14:32Z", &zone_named("utc").unwrap()).unwrap(), &tokyo);
        assert_eq!(shown.strftime("%H:%M").to_string(), "23:32");
    }

    #[test]
    fn a_zone_no_one_knows_is_told_the_nearest() {
        assert_eq!(
            zone_named("Europe/Mosow").unwrap_err(),
            "no such zone: an IANA name as Asia/Tokyo, utc or local; did you mean Europe/Moscow?"
        );
        assert!(zone_named("local").is_ok() && zone_named("Etc/UTC").is_ok());
    }
}
