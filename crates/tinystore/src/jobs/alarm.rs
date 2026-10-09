//! A queue's next time in memory, so that nothing polls and a million jobs
//! due next week cost nothing until next week.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

/// The longest a worker sleeps without looking at its clock again, so that a
/// clock that jumps is met.
pub(crate) const LONGEST_SLEEP: Duration = Duration::from_secs(60);

/// What a write that makes a job due sooner wakes.
pub(crate) trait Wake: Send + Sync {
    fn wake(&self);
}

/// A worker sets the alarm from the file when a claim found fewer jobs than it
/// wanted; a write that makes a job due sooner lowers it and wakes every
/// worker waiting on it:
///
/// ```text
/// claim finds 3 of 8, the next due at 18:00   → at 18:00, workers sleep until then
/// an add at 17:30 commits                     → at 17:30, the workers wake and sleep again
/// 18:00, nothing lowered it                   → the workers claim
/// ```
pub(crate) struct Alarm {
    state: Mutex<State>,
}

struct State {
    /// Unix milliseconds; `i64::MAX` when nothing waits.
    at: i64,
    /// Each read of the file under way, and the earliest time a write committed
    /// while it read, which the read may not have seen.
    reads: HashMap<u64, i64>,
    last_read: u64,
    waiters: Vec<Weak<dyn Wake>>,
}

/// One worker's read of the file.
#[derive(Debug)]
pub(crate) struct Read(u64);

impl Alarm {
    /// An alarm that rings at once, so that the first worker reads the file.
    pub(crate) fn new() -> Alarm {
        Alarm { state: Mutex::new(State { at: i64::MIN, reads: HashMap::new(), last_read: 0, waiters: Vec::new() }) }
    }

    /// Called by a write once it committed a job due at `at`.
    pub(crate) fn lower(&self, at: i64) {
        let mut state = self.lock();
        for lowered in state.reads.values_mut() {
            *lowered = (*lowered).min(at);
        }
        if at >= state.at {
            return;
        }
        state.at = at;
        state.waiters.retain(|waiter| waiter.strong_count() > 0);
        let waiters: Vec<_> = state.waiters.iter().filter_map(Weak::upgrade).collect();
        drop(state);
        for waiter in waiters {
            waiter.wake();
        }
    }

    /// Starts a read of the file when a job may be due at `now`.
    pub(crate) fn rung(&self, now: i64) -> Option<Read> {
        let mut state = self.lock();
        if now < state.at {
            return None;
        }
        state.last_read += 1;
        let read = state.last_read;
        state.reads.insert(read, i64::MAX);
        Some(Read(read))
    }

    pub(crate) fn due(&self, now: i64) -> bool {
        now >= self.lock().at
    }

    /// Ends a read that found the queue's next time, none for never; a write
    /// committed while it read keeps its time when it is sooner:
    ///
    /// ```text
    /// read starts, an add at 17:30 commits, read finds 18:00   → at 17:30
    /// read starts, an add at 19:00 commits, read finds 18:00   → at 18:00
    /// ```
    pub(crate) fn set(&self, read: Read, next: Option<i64>) {
        let mut state = self.lock();
        let lowered = state.reads.remove(&read.0).unwrap_or(i64::MAX);
        state.at = next.unwrap_or(i64::MAX).min(lowered);
    }

    /// Ends a read that found no next time: the alarm keeps ringing.
    pub(crate) fn forget(&self, read: Read) {
        self.lock().reads.remove(&read.0);
    }

    /// How long a worker sleeps at `now` before it looks again.
    pub(crate) fn sleep(&self, now: i64) -> Duration {
        let at = self.lock().at;
        if at == i64::MAX {
            return LONGEST_SLEEP;
        }
        let ms = u64::try_from(at.saturating_sub(now)).unwrap_or(0);
        Duration::from_millis(ms).min(LONGEST_SLEEP)
    }

    pub(crate) fn subscribe(&self, waiter: Weak<dyn Wake>) {
        self.lock().waiters.push(waiter);
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for Alarm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Alarm({})", self.lock().at)
    }
}
