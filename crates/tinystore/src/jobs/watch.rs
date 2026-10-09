//! Watching a job: the job as it is, then again each time it changes, until
//! it ends. Every change a queue commits wakes its watchers, and each reads
//! its own job again. A job found gone was done, unless a cancel marked its
//! watchers before it committed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::alarm::LONGEST_SLEEP;
use super::queue::Queue;
use super::read::{Job, State};
use super::state::QueueState;
use crate::Result;

/// What wakes one watcher: it returns at once and calls nothing of the engine.
pub(crate) type Hears = Arc<dyn Fn() + Send + Sync>;

/// A queue's watchers, by the number of their subscription.
#[derive(Default)]
pub(crate) struct Watchers {
    subs: Mutex<HashMap<u64, Sub>>,
    last: AtomicU64,
}

struct Sub {
    key: String,
    hears: Hears,
    cancelled: bool,
}

impl Watchers {
    /// Wakes every watcher of the queue: what it committed may have changed
    /// their jobs, or where their jobs stand in it.
    pub(crate) fn changed(&self) {
        let hearing: Vec<Hears> = self.lock().values().map(|sub| Arc::clone(&sub.hears)).collect();
        for hears in hearing {
            hears();
        }
    }

    /// Marks the watchers of `key` as watching a cancelled job, or takes the
    /// mark back when the cancel failed. A cancel marks them inside its
    /// write, before it commits: a watcher woken by another commit meanwhile
    /// could find the job gone and take it for done.
    pub(crate) fn cancelled(&self, key: &str, cancelled: bool) {
        for sub in self.lock().values_mut().filter(|sub| sub.key == key) {
            sub.cancelled = cancelled;
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<u64, Sub>> {
        self.subs.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for Watchers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Watchers({})", self.lock().len())
    }
}

/// A watcher's place among its queue's, given up when it is dropped.
pub(crate) struct Subscription {
    queue: Arc<QueueState>,
    id: u64,
}

impl Subscription {
    pub(crate) fn new(queue: &Arc<QueueState>, key: &str, hears: Hears) -> Subscription {
        let id = queue.watchers.last.fetch_add(1, Ordering::Relaxed) + 1;
        queue.watchers.lock().insert(id, Sub { key: key.to_owned(), hears, cancelled: false });
        Subscription { queue: Arc::clone(queue), id }
    }

    fn cancelled(&self) -> bool {
        self.queue.watchers.lock().get(&self.id).is_some_and(|sub| sub.cancelled)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.queue.watchers.lock().remove(&self.id);
    }
}

/// What reading a watched job again gives.
pub(crate) enum Report<V> {
    /// The job as it is now, which changed since the last report.
    Changed(Job<V>),
    /// Nothing a watcher sees changed.
    Same,
    /// The watch is over: it reported its job's end, or no job had its id.
    Over,
}

/// What a watcher reported last, so that it reports a job again only when it
/// changed.
pub(crate) struct Seen<V> {
    last: Option<Job<V>>,
    over: bool,
}

impl<V> Default for Seen<V> {
    fn default() -> Self {
        Seen { last: None, over: false }
    }
}

impl<V> Seen<V>
where
    V: Serialize + DeserializeOwned + Clone + Send + 'static,
{
    /// Reads the job under `key` again and says what to report. A job found
    /// gone is reported once more, as it was last seen, done or cancelled.
    pub(crate) fn read(&mut self, queue: &Queue<V>, key: &str, sub: &Subscription) -> Result<Report<V>> {
        if self.over {
            return Ok(Report::Over);
        }
        let Some(job) = queue.get(key)? else {
            self.over = true;
            return Ok(self.last.take().map_or(Report::Over, |mut last| {
                last.state = if sub.cancelled() { State::Cancelled } else { State::Done };
                last.ahead = 0;
                Report::Changed(last)
            }));
        };
        if self.last.as_ref().is_some_and(|last| shows_the_same(last, &job)) {
            return Ok(Report::Same);
        }
        self.over = matches!(job.state, State::Done | State::Failed);
        self.last = Some(job.clone());
        Ok(Report::Changed(job))
    }
}

/// Whether two readings of a job show a watcher the same: its state, place,
/// attempt, time, progress and error.
fn shows_the_same<V>(a: &Job<V>, b: &Job<V>) -> bool {
    (a.state, a.ahead, a.attempt, a.at) == (b.state, b.ahead, b.attempt, b.at)
        && a.progress == b.progress
        && a.error == b.error
}

/// A job watched from Rust: the job as it is, then again each time it
/// changes, until it ends; each `next` waits on the calling thread. Dropped,
/// it stops watching.
pub struct Watch<V> {
    queue: Queue<V>,
    key: String,
    woken: Arc<Woken>,
    sub: Subscription,
    seen: Seen<V>,
}

/// A flag a change raises and a watcher waits for.
#[derive(Default)]
struct Woken {
    raised: Mutex<bool>,
    changed: Condvar,
}

impl Woken {
    fn raise(&self) {
        *self.raised.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
    }

    /// Waits until a change raised the flag, and lowers it; a wait looks
    /// again after a while, as every wait for a time does.
    fn wait(&self) {
        let raised = self.raised.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut raised, _) = self
            .changed
            .wait_timeout_while(raised, LONGEST_SLEEP, |raised| !*raised)
            .unwrap_or_else(PoisonError::into_inner);
        *raised = false;
    }
}

impl<V> Queue<V>
where
    V: Serialize + DeserializeOwned + Clone + Send + 'static,
{
    /// The job under `id` as it is, then again each time its state, place,
    /// attempt, time, progress or error changes; it ends with the job: done,
    /// failed or cancelled, the last it yields. An id with no job yields
    /// nothing.
    pub fn watch(&self, id: impl AsRef<str>) -> Watch<V> {
        let woken = Arc::new(Woken::default());
        let raising = Arc::clone(&woken);
        let sub = Subscription::new(&self.state, id.as_ref(), Arc::new(move || raising.raise()));
        Watch { queue: self.clone(), key: id.as_ref().to_owned(), woken, sub, seen: Seen::default() }
    }
}

impl<V> Iterator for Watch<V>
where
    V: Serialize + DeserializeOwned + Clone + Send + 'static,
{
    type Item = Result<Job<V>>;

    fn next(&mut self) -> Option<Result<Job<V>>> {
        loop {
            match self.seen.read(&self.queue, &self.key, &self.sub) {
                Ok(Report::Changed(job)) => return Some(Ok(job)),
                Ok(Report::Over) => return None,
                Ok(Report::Same) => self.woken.wait(),
                Err(error) => {
                    self.seen.over = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

impl<V> std::fmt::Debug for Watch<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Watch({}: id {:?})", self.queue.state.describe(), self.key)
    }
}
