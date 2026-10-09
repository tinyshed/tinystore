//! When a job runs again: an interval, each id at a phase of its own within
//! it, or a cron expression on a named zone's wall clock. A repeat is kept as
//! text, so that another language reads the same schedule:
//!
//! ```text
//! every 30 s, id probe:7     @every 30s +6178ms
//! cron 10 3 * * *, Berlin    10 3 * * * Europe/Berlin
//! ```

use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::cron::Cron;
use crate::clock::millis;
use crate::{Error, Result};

/// The longest a repeat's text may be.
const MAX_TEXT: usize = 1024;

/// A repeat a call asked for, before it is a job's: an interval's phase comes
/// with the job's id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Asked {
    Every(Duration),
    Cron { expr: String, zone: String },
}

/// A job's repeat, read from its text or made for its id.
#[derive(Clone, Debug)]
pub(crate) enum Repeat {
    Every { every: i64, phase: i64 },
    Cron { cron: Cron, zone: TimeZone, name: String },
}

impl Asked {
    /// Checks what was asked: an interval of a second at least, a cron that
    /// reads and runs, and a zone the database knows.
    #[cfg(test)]
    pub(crate) fn check(&self) -> Result<()> {
        self.of("").map(drop)
    }

    /// The repeat a job under `id` keeps: an interval takes the id's phase.
    pub(crate) fn of(&self, id: &str) -> Result<Repeat> {
        match self {
            Asked::Every(every) => {
                let every = millis(*every);
                if every < 1000 {
                    return Err(Error::invalid(format!("every {every} ms: a repeat is a second at least")));
                }
                Ok(Repeat::Every { every, phase: phase_of(id, every) })
            }
            Asked::Cron { expr, zone } => {
                let repeat = Repeat::cron(expr, zone)?;
                if repeat.text().len() > MAX_TEXT {
                    return Err(Error::invalid(format!("a repeat of more than {MAX_TEXT} bytes")));
                }
                Ok(repeat)
            }
        }
    }
}

impl Repeat {
    fn cron(expr: &str, zone: &str) -> Result<Repeat> {
        let cron = Cron::parse(expr).map_err(|why| Error::invalid(format!("cron {expr:?}: {why}")))?;
        if zone.is_empty() {
            return Err(Error::invalid(format!("cron {expr:?}: a cron needs a time zone, \"UTC\" among them")));
        }
        let found = TimeZone::get(zone).map_err(|_| Error::invalid(format!("time zone {zone:?}: no such zone")))?;
        let repeat = Repeat::Cron { cron, zone: found, name: zone.to_owned() };
        if repeat.next(0).is_none() {
            return Err(Error::invalid(format!("cron {expr:?}: it never runs")));
        }
        Ok(repeat)
    }

    /// Reads a repeat as a job keeps it.
    pub(crate) fn parse(text: &str) -> Result<Repeat> {
        let corrupt = |why: &str| Error::corrupt(format!("a kept repeat {text:?}: {why}"));
        if let Some(interval) = text.strip_prefix("@every ") {
            let (every, phase) = match interval.split_once(" +") {
                Some((every, phase)) => (parse_interval(every), parse_interval(phase)),
                None => (parse_interval(interval), Some(0)),
            };
            return match (every, phase) {
                (Some(every), Some(phase)) if every >= 1000 && phase < every => Ok(Repeat::Every { every, phase }),
                _ => Err(corrupt("not an interval and a phase within it")),
            };
        }
        let (expr, zone) = text.rsplit_once(' ').ok_or_else(|| corrupt("no zone"))?;
        Repeat::cron(expr, zone).map_err(|error| corrupt(&error.to_string()))
    }

    /// The text a job keeps.
    pub(crate) fn text(&self) -> String {
        match self {
            Repeat::Every { every, phase: 0 } => format!("@every {}", spell(*every)),
            Repeat::Every { every, phase } => format!("@every {} +{}", spell(*every), spell(*phase)),
            Repeat::Cron { cron, name, .. } => format!("{} {name}", cron.spelling),
        }
    }

    /// The first time after `after`, unix milliseconds, that the repeat runs;
    /// none for a cron that never runs again.
    pub(crate) fn next(&self, after: i64) -> Option<i64> {
        match self {
            Repeat::Every { every, phase } => Some(((after - phase).div_euclid(*every) + 1) * every + phase),
            Repeat::Cron { cron, zone, .. } => {
                let after = Timestamp::from_millisecond(after).ok()?;
                cron.next(zone, after).map(|next| next.as_millisecond())
            }
        }
    }
}

/// An id's place within an interval: its FNV-1a hash modulo the interval, so
/// that the jobs of many ids repeating together do not all run in one instant.
fn phase_of(id: &str, every: i64) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let every = u64::try_from(every).unwrap_or(1).max(1);
    i64::try_from(hash % every).unwrap_or(0)
}

/// Milliseconds in their largest whole unit, as every language reads them
/// back: 15m, 90s, 1500ms.
fn spell(ms: i64) -> String {
    for (size, unit) in [(3_600_000, "h"), (60_000, "m"), (1000, "s")] {
        if ms % size == 0 {
            return format!("{}{unit}", ms / size);
        }
    }
    format!("{ms}ms")
}

fn parse_interval(text: &str) -> Option<i64> {
    for (unit, size) in [("ms", 1), ("h", 3_600_000), ("m", 60_000), ("s", 1000)] {
        if let Some(number) = text.strip_suffix(unit) {
            return number.parse::<i64>().ok().filter(|n| *n >= 0).and_then(|n| n.checked_mul(size));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interval_gives_each_id_a_phase_of_its_own_kept_in_its_text() {
        let repeat = Asked::Every(Duration::from_secs(30)).of("probe:7").unwrap();
        let text = repeat.text();
        assert!(text.starts_with("@every 30s +"), "{text}");
        let Repeat::Every { phase, .. } = Repeat::parse(&text).unwrap() else { panic!("{text}") };
        assert!((0..30_000).contains(&phase));
        // each run falls at the same place in its interval
        let first = repeat.next(1_800_000_000_000).unwrap();
        assert_eq!((first - phase) % 30_000, 0);
        assert_eq!(repeat.next(first), Some(first + 30_000));
        assert_ne!(phase, phase_of("probe:8", 30_000), "two ids, two phases");
    }

    #[test]
    fn a_cron_keeps_its_zone_in_its_text() {
        let repeat = Asked::Cron { expr: "10 3 * * *".into(), zone: "Europe/Berlin".into() }.of("x").unwrap();
        assert_eq!(repeat.text(), "10 3 * * * Europe/Berlin");
        let again = Repeat::parse(&repeat.text()).unwrap();
        assert_eq!(again.text(), repeat.text());
    }

    #[test]
    fn a_repeat_is_refused_for_what_it_lacks() {
        let error = Asked::Cron { expr: "0 9 * * *".into(), zone: String::new() }.check().unwrap_err();
        assert!(error.to_string().contains("needs a time zone"), "{error}");
        let error = Asked::Cron { expr: "0 9 * * *".into(), zone: "Mars/Olympus".into() }.check().unwrap_err();
        assert!(error.to_string().contains("no such zone"), "{error}");
        let error = Asked::Cron { expr: "0 0 31 2 *".into(), zone: "UTC".into() }.check().unwrap_err();
        assert!(error.to_string().contains("never runs"), "{error}");
        assert!(Asked::Every(Duration::from_millis(500)).check().is_err());
    }

    #[test]
    fn a_kept_repeat_that_does_not_read_is_corrupt() {
        for text in ["@every 30s +31s", "@every soon", "10 3 * * *"] {
            assert_eq!(Repeat::parse(text).unwrap_err().kind(), crate::ErrorKind::Corrupt, "{text}");
        }
    }
}
