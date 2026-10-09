//! A queue as this process knows it, shared by every handle on it: its id and
//! options, its alarm and rate, and the jobs its leases hold.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::alarm::Alarm;
use super::claim::Lease;
use super::policy::Policy;
use super::rate::RateLog;
use super::watch::Watchers;

/// What a name is: a queue of jobs, or a schedule's one repeating job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Queue,
    Schedule,
}

impl Kind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Kind::Queue => "queue",
            Kind::Schedule => "schedule",
        }
    }
}

/// A repeated event is logged at most once in this long, with how many there
/// were since.
const QUIET: Duration = Duration::from_secs(10 * 60);

#[derive(Debug)]
pub(crate) struct QueueState {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) kind: Kind,
    pub(crate) policy: Policy,
    pub(crate) alarm: Alarm,
    pub(crate) rate: Option<RateLog>,
    pub(crate) watchers: Watchers,
    /// The jobs this process's leases hold, by job id: running, or claimed for
    /// a handler still busy with the one before.
    held: Mutex<HashMap<i64, Arc<Lease>>>,
    /// Where maintenance's walk over the keys jobs left behind goes on.
    pub(crate) keys_after: Mutex<String>,
    pub(crate) failures: Quiet,
    pub(crate) panics: Quiet,
    pub(crate) lost: Quiet,
    pub(crate) writes: Quiet,
}

impl QueueState {
    pub(crate) fn new(id: i64, name: &str, kind: Kind, policy: Policy) -> QueueState {
        QueueState {
            id,
            name: name.to_owned(),
            kind,
            policy,
            alarm: Alarm::new(),
            rate: policy.rate.map(|(count, per)| RateLog::new(count, per)),
            watchers: Watchers::default(),
            held: Mutex::new(HashMap::new()),
            keys_after: Mutex::new(String::new()),
            failures: Quiet::new("jobs failed for good"),
            panics: Quiet::new("a handler panicked"),
            lost: Quiet::new("a worker lost the lease of a job it ran"),
            writes: Quiet::new("a worker's write failed, and it tries again"),
        }
    }

    /// What an error about this queue says first: `jobs queue emails`.
    pub(crate) fn describe(&self) -> String {
        format!("jobs {} {}", self.kind.as_str(), self.name)
    }

    pub(crate) fn hold(&self, lease: &Arc<Lease>) {
        lock(&self.held).insert(lease.id, Arc::clone(lease));
    }

    /// Lets go of a lease, unless another of the same job took its place.
    pub(crate) fn release(&self, lease: &Lease) {
        let mut held = lock(&self.held);
        if held.get(&lease.id).is_some_and(|kept| std::ptr::eq(kept.as_ref(), lease)) {
            held.remove(&lease.id);
        }
    }

    pub(crate) fn held(&self, id: i64) -> Option<Arc<Lease>> {
        lock(&self.held).get(&id).cloned()
    }

    /// The running jobs whose rows lie before a place in the order the queue
    /// runs them: the rows before a job, less these, wait before it. A lease
    /// that has ended runs nothing, though its holder never settled it.
    pub(crate) fn running_before(&self, next: i64, id: i64, now: i64) -> usize {
        lock(&self.held).values().filter(|lease| (lease.next, lease.id) < (next, id) && lease.running(now)).count()
    }

    /// Wakes the workers of a queue that bounds what runs once a settlement
    /// gave a place back, since a claim that found none waits for that.
    pub(crate) fn room_made(&self, now: i64) {
        if self.policy.bounded() {
            self.alarm.lower(now);
        }
    }
}

/// One kind of event of one queue, logged at most once a quiet period with how
/// many happened since, so that a million failures are not a million lines:
///
/// ```text
/// 00:00  a job failed for good   → warn count=1
/// 00:01…00:09 1,204 more         → counted
/// 00:10  one more                → warn count=1205
/// ```
#[derive(Debug)]
pub(crate) struct Quiet {
    message: &'static str,
    state: Mutex<(usize, Option<Instant>)>,
}

impl Quiet {
    fn new(message: &'static str) -> Quiet {
        Quiet { message, state: Mutex::new((0, None)) }
    }

    pub(crate) fn observe(&self, queue: &str, last: &str) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.0 += 1;
        if state.1.is_some_and(|logged| logged.elapsed() < QUIET) {
            return;
        }
        let count = state.0;
        *state = (0, Some(Instant::now()));
        drop(state);
        tracing::warn!(target: "tinystore", queue, count, last, "{}", self.message);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
