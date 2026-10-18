use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{Error, Result};

/// Where a store reads the time. Every engine reads it through the store, so a
/// test that moves one clock moves them all.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> SystemTime;
}

/// The system's clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// A clock a test moves by hand. It never moves backwards: engines keep
/// watermarks and leases by it, and a step back would make them lie.
pub struct TestClock {
    now: Mutex<SystemTime>,
}

impl TestClock {
    pub fn new(start: SystemTime) -> Self {
        Self { now: Mutex::new(start) }
    }

    pub fn advance(&self, by: Duration) {
        let mut now = self.now.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *now += by;
    }

    pub fn set(&self, to: SystemTime) -> Result<()> {
        let mut now = self.now.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if to < *now {
            return Err(Error::invalid(format!(
                "test clock: {} ms is before its time, {} ms",
                unix_millis(to),
                unix_millis(*now)
            )));
        }
        *now = to;
        Ok(())
    }
}

impl Clock for TestClock {
    fn now(&self) -> SystemTime {
        *self.now.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl fmt::Debug for TestClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TestClock({} ms)", unix_millis(self.now()))
    }
}

/// Milliseconds since the Unix epoch; negative before it.
pub fn unix_millis(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since) => saturate(since.as_millis()),
        Err(before) => -saturate(before.duration().as_millis()),
    }
}

/// Nanoseconds since the Unix epoch; negative before it.
pub fn unix_nanos(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since) => saturate(since.as_nanos()),
        Err(before) => -saturate(before.duration().as_nanos()),
    }
}

/// The time `millis` milliseconds after the Unix epoch; before it when negative.
/// A time outside the years 1601 to 9999 is held at their edge: `SystemTime`
/// panics past what the platform represents, and Windows' begins at 1601.
#[cfg(any(feature = "kv", feature = "jobs", feature = "metrics"))]
pub(crate) fn from_unix_millis(millis: i64) -> SystemTime {
    const LATEST: Duration = Duration::from_secs(253_402_300_799);
    const EARLIEST: Duration = Duration::from_secs(11_644_473_600);
    match u64::try_from(millis) {
        Ok(after) => UNIX_EPOCH + Duration::from_millis(after).min(LATEST),
        Err(_) => UNIX_EPOCH - Duration::from_millis(millis.unsigned_abs()).min(EARLIEST),
    }
}

/// The time `nanos` nanoseconds after the Unix epoch; the epoch for a time
/// before it.
#[cfg(feature = "records")]
pub(crate) fn from_unix_nanos(nanos: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(nanos.max(0) as u64)
}

/// A span in whole milliseconds, saturating.
#[cfg(any(
    feature = "kv",
    feature = "jobs",
    feature = "blobs",
    feature = "records",
    feature = "metrics",
    feature = "replication"
))]
pub(crate) fn millis(span: Duration) -> i64 {
    saturate(span.as_millis())
}

/// The year, month and day `days` after 1970-01-01, by Howard Hinnant's
/// `civil_from_days`, its years counted in eras of 400.
#[cfg(any(feature = "config", feature = "records", feature = "metrics"))]
pub(crate) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let of_era = shifted.rem_euclid(146_097);
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * of_year + 2) / 153;
    let day = of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

fn saturate(value: u128) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_clock_moves_only_forward() {
        let start = UNIX_EPOCH + Duration::from_secs(1_000);
        let clock = TestClock::new(start);
        clock.advance(Duration::from_millis(250));
        assert_eq!(unix_millis(clock.now()), 1_000_250);

        let error = clock.set(start).unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Invalid);
        assert_eq!(unix_millis(clock.now()), 1_000_250);
    }

    #[test]
    fn a_time_before_the_epoch_is_negative() {
        let before = UNIX_EPOCH - Duration::from_millis(1_500);
        assert_eq!(unix_millis(before), -1_500);
        assert_eq!(unix_nanos(before), -1_500_000_000);
    }

    #[test]
    #[cfg(any(feature = "kv", feature = "jobs", feature = "metrics"))]
    fn a_time_past_any_platform_is_held_at_its_edge() {
        assert!(from_unix_millis(i64::MAX) > from_unix_millis(0));
        assert!(from_unix_millis(i64::MIN) < from_unix_millis(0));
        assert_eq!(unix_millis(from_unix_millis(-1_500)), -1_500);
    }
}
