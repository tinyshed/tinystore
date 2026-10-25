//! The starts a queue's rate counts, and each group's under its group rate,
//! in memory: a restart forgets them.

use std::collections::{HashMap, VecDeque};
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

    /// Whether no start it counted is left in the span at `now`.
    fn idle(&self, now: i64) -> bool {
        let mut state = self.lock();
        self.forget(&mut state, now);
        state.used == 0
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

/// A log of each group that started a job within the last span, past which
/// the logs of the groups idle since are let go.
const GROUP_LOGS: usize = 1024;

/// Each group's starts under a queue's group rate. A job of a group whose
/// rate lets none start waits for its turn under the rate, rather than for a
/// claim that meets it again: it moves out of the way of every other group's
/// jobs, and is due again when its group may start it.
#[derive(Debug)]
pub(crate) struct GroupRates {
    count: u32,
    per: Duration,
    groups: Mutex<Groups>,
}

#[derive(Debug, Default)]
struct Groups {
    by_name: HashMap<String, Group>,
    /// How many groups the map may hold before the idle ones are let go:
    /// twice what the last sweep left, so that a sweep costs a take little.
    sweep_at: usize,
}

/// A group's starts, and the turn its waiting jobs were last given.
#[derive(Debug)]
struct Group {
    log: RateLog,
    /// The latest turn given to a job that waits, and how many it was given:
    /// a job that comes to wait after them takes the turn after theirs.
    turn: i64,
    given: usize,
}

impl GroupRates {
    pub(crate) fn new(count: u32, per: Duration) -> GroupRates {
        GroupRates { count, per, groups: Mutex::default() }
    }

    /// Counts a start of `group` at `now`; when its rate lets none start,
    /// gives the job its turn: the next its group has, past those given to
    /// the jobs that wait already, its rate's count a turn, so that a job
    /// waits once.
    ///
    /// ```text
    /// 2 a second, starts at 0 and 0 ms   jobs that wait: 1000, 1000, 2000, 2000, 3000 ms
    /// ```
    pub(crate) fn take(&self, group: &str, now: i64) -> Result<(), i64> {
        let mut groups = self.groups.lock().unwrap_or_else(PoisonError::into_inner);
        if groups.by_name.len() >= groups.sweep_at.max(GROUP_LOGS) {
            groups.by_name.retain(|_, group| !group.log.idle(now) || group.turn > now);
            groups.sweep_at = groups.by_name.len() * 2;
        }
        let entry = groups.by_name.entry(group.to_owned());
        let group = entry.or_insert_with(|| Group { log: RateLog::new(self.count, self.per), turn: 0, given: 0 });
        if group.log.take(now, 1) == 1 {
            return Ok(());
        }
        let first = group.log.room(now).1.unwrap_or(now);
        if group.turn < first {
            (group.turn, group.given) = (first, 0);
        } else if group.given == group.log.count {
            (group.turn, group.given) = (group.turn.saturating_add(group.log.per), 0);
        }
        group.given += 1;
        Err(group.turn)
    }

    /// Uncounts the start the last take of `group` counted, which its claim
    /// did not lease.
    pub(crate) fn give_back(&self, group: &str) {
        let groups = self.groups.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(group) = groups.by_name.get(group) {
            group.log.give_back(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // contract: JOB-09
    #[test]
    fn a_job_that_waits_for_its_groups_turn_takes_the_one_after_those_waiting_already() {
        let rates = GroupRates::new(2, Duration::from_secs(1));
        assert_eq!((rates.take("bot", 0), rates.take("bot", 0)), (Ok(()), Ok(())));
        let turns: Vec<_> = (0..5).map(|_| rates.take("bot", 0)).collect();
        assert_eq!(turns, [Err(1000), Err(1000), Err(2000), Err(2000), Err(3000)]);
        assert_eq!(rates.take("bot", 10), Err(3000), "a job that comes later waits behind them, once");
        assert_eq!(rates.take("other", 10), Ok(()), "another group has its own");
    }

    // contract: JOB-09
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

    // contract: JOB-09
    #[test]
    fn starts_given_back_start_again() {
        let rate = RateLog::new(2, Duration::from_secs(1));
        assert_eq!(rate.take(0, 2), 2);
        rate.give_back(1);
        assert_eq!(rate.room(1), (1, None));
    }
}
