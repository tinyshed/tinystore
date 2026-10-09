//! The starts a queue's rate counts, in memory: a restart forgets them.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::clock::millis;

/// When claims leased jobs and how many, the oldest first, within the last
/// span.
///
/// A claim within a 1024th of the span after the last is counted at the later
/// time, so that the log holds about 1024 entries whatever the rate. A start
/// then stays in the span a little longer than it was, which never lets more
/// through:
///
/// ```text
/// 3 a second, starts at 0, 0 and 400 ms   two more from 1000 ms, a third from 1400 ms
/// ```
#[derive(Debug)]
pub(crate) struct RateLog {
    count: usize,
    per: i64,
    state: Mutex<Starts>,
}

#[derive(Debug, Default)]
struct Starts {
    /// Unix milliseconds and how many started then.
    starts: VecDeque<(i64, usize)>,
    used: usize,
    /// The latest time asked about, so that a clock stepping back moves nothing.
    last: i64,
}

impl RateLog {
    pub(crate) fn new(count: u32, per: Duration) -> RateLog {
        let count = usize::try_from(count).unwrap_or(usize::MAX);
        RateLog { count, per: millis(per).max(1), state: Mutex::new(Starts::default()) }
    }

    /// How many jobs may start at `now`, and when one may next when none can.
    pub(crate) fn room(&self, now: i64) -> (usize, Option<i64>) {
        let mut state = self.lock();
        self.forget(&mut state, now);
        let left = self.count.saturating_sub(state.used);
        if left > 0 {
            return (left, None);
        }
        (0, state.starts.front().map(|(at, _)| at + self.per))
    }

    /// Counts up to `want` starts at `now`, and says how many it counted.
    pub(crate) fn take(&self, now: i64, want: usize) -> usize {
        let mut state = self.lock();
        let now = self.forget(&mut state, now);
        let taken = want.min(self.count.saturating_sub(state.used));
        if taken == 0 {
            return 0;
        }
        match state.starts.back_mut() {
            Some((at, started)) if *at >= now - self.per / 1024 => (*at, *started) = (now, *started + taken),
            _ => state.starts.push_back((now, taken)),
        }
        state.used += taken;
        taken
    }

    /// Uncounts `n` of the starts the last take counted, which its claim did
    /// not lease. Claims take and give back inside the file's one writer, so
    /// that no take comes between.
    pub(crate) fn give_back(&self, n: usize) {
        let mut state = self.lock();
        let Some((_, started)) = state.starts.back_mut() else {
            return; // the span has passed since, and took the starts along
        };
        let n = n.min(*started);
        *started -= n;
        if *started == 0 {
            state.starts.pop_back();
        }
        state.used -= n;
    }

    /// Drops the starts a span old at `now`, and answers `now`, or the latest
    /// time asked about when the clock stepped back.
    fn forget(&self, state: &mut Starts, now: i64) -> i64 {
        let now = now.max(state.last);
        state.last = now;
        while let Some(&(at, started)) = state.starts.front() {
            if at > now - self.per {
                break;
            }
            state.used -= started;
            state.starts.pop_front();
        }
        now
    }

    fn lock(&self) -> MutexGuard<'_, Starts> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_never_lets_more_than_its_count_start_in_a_span() {
        let rate = RateLog::new(3, Duration::from_secs(1));
        assert_eq!(rate.take(0, 2), 2);
        assert_eq!(rate.take(400, 5), 1);
        assert_eq!(rate.room(999), (0, Some(1000)));
        assert_eq!(rate.take(1000, 5), 2, "the two of 0 ms left the span");
        assert_eq!(rate.take(1399, 5), 0);
        assert_eq!(rate.take(1400, 5), 1);
    }

    #[test]
    fn starts_given_back_start_again() {
        let rate = RateLog::new(2, Duration::from_secs(1));
        assert_eq!(rate.take(0, 2), 2);
        rate.give_back(1);
        assert_eq!(rate.room(1), (1, None));
    }
}
