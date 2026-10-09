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
}
