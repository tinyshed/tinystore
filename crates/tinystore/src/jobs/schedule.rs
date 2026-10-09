//! Schedules: a repeat the code owns, one repeating job under the schedule's
//! name, with the handler that runs it.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;

use super::policy::Policy;
use super::queue::Queue;
use super::read::Job;
use super::repeat::{Asked, Repeat};
use super::rows::{self, Change, NewJob};
use super::run::{Outcome, Run};
use super::state::{Kind, QueueState};
use super::values::{self, Kept};
use super::work::Worker;
use crate::{Error, Result, Store};

/// A schedule being opened: its name and its repeat.
#[must_use = "a schedule opens with work() or open()"]
pub struct ScheduleBuilder {
    store: Store,
    name: String,
    repeat: Option<Asked>,
    policy: Policy,
}

impl Store {
    /// A schedule: one repeating job under `name`, the code's own. Each time
    /// the program opens it, the code's repeat replaces the one kept.
    pub fn schedule(&self, name: &str) -> ScheduleBuilder {
        ScheduleBuilder { store: self.clone(), name: name.to_owned(), repeat: None, policy: Policy::default() }
    }
}

impl fmt::Debug for ScheduleBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScheduleBuilder").field("name", &self.name).field("repeat", &self.repeat).finish()
    }
}

impl ScheduleBuilder {
    /// Runs every `span`, at the schedule's own phase within it.
    pub fn every(mut self, span: Duration) -> Self {
        self.repeat = Some(Asked::Every(span));
        self
    }

    /// Runs by a five-field cron expression on the wall clock of `zone`, an
    /// IANA name, `"UTC"` among them.
    pub fn cron(mut self, expr: &str, zone: &str) -> Self {
        self.repeat = Some(Asked::Cron { expr: expr.to_owned(), zone: zone.to_owned() });
        self
    }

    /// Runs a run gets before it fails, the first counted: 10. A failed run
    /// retries, never past the next time, and the schedule goes on.
    pub fn attempts(mut self, attempts: u32) -> Self {
        self.policy.attempts = attempts;
        self
    }

    pub fn backoff(mut self, initial: Duration, max: Duration) -> Self {
        self.policy.backoff = (initial, max);
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.policy.timeout = timeout;
        self
    }

    /// Opens the schedule and runs `handler` at each time on a worker of the
    /// store's, as `Queue::work` does; the handler gets the run alone.
    pub fn work<F, R, E>(self, handler: F) -> Result<Schedule>
    where
        F: Fn(&Run) -> std::result::Result<R, E> + Send + Sync + 'static,
        R: Into<Outcome>,
        E: fmt::Display,
    {
        let mut schedule = self.open()?;
        schedule.worker = Some(schedule.queue.work(move |(): (), run: &Run| handler(run))?);
        Ok(schedule)
    }

    /// Opens the schedule without running it: for a program that reads where
    /// it is, or a test that runs it with `run_due`.
    pub fn open(self) -> Result<Schedule> {
        let describe = || format!("jobs schedule {}", self.name);
        let asked = self.repeat.ok_or_else(|| Error::invalid("a schedule needs every or cron").within(describe()))?;
        let repeat = asked.of(&self.name).map_err(|error| error.within(describe()))?;
        let queue = Queue::<()>::open(&self.store, &self.name, Kind::Schedule, self.policy)?;
        keep_repeat(&queue, &repeat)?;
        Ok(Schedule { queue, worker: None })
    }
}

/// A repeat the code owns, and the worker running it when it was opened with
/// one.
pub struct Schedule {
    queue: Queue<()>,
    worker: Option<Worker>,
}

impl Schedule {
    /// When it runs next and how its last run went: what a page of cron jobs
    /// shows.
    pub fn get(&self) -> Result<Option<Job<()>>> {
        self.queue.get(&self.queue.state.name)
    }

    /// Runs what is due, as `Queue::run_due` does.
    pub fn run_due<F, R, E>(&self, handler: F) -> Result<usize>
    where
        F: Fn(&Run) -> std::result::Result<R, E> + Sync,
        R: Into<Outcome>,
        E: fmt::Display,
    {
        self.queue.run_due(|(): (), run: &Run| handler(run))
    }

    /// Stops the schedule's worker, as `Worker::stop` does.
    pub fn stop(&self) {
        if let Some(worker) = &self.worker {
            worker.stop();
        }
    }

    /// Removes the schedule's job, until the program opens it again.
    pub fn cancel(&self) -> Result<bool> {
        self.queue.cancel(&self.queue.state.name)
    }
}

impl fmt::Debug for Schedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.queue.state.describe())
    }
}

/// Makes the schedule's one job repeat as the code says: added at its next
/// time when it is not there, given the code's repeat when it keeps another.
/// A job a run holds keeps its row, the new repeat taking over after the run.
fn keep_repeat(queue: &Queue<()>, repeat: &Repeat) -> Result<()> {
    let now = queue.jobs.now();
    let next = repeat.next(now).ok_or_else(|| Error::invalid("the repeat never runs again"))?;
    let (id, text, value) = (queue.jobs.next_id()?, repeat.text(), Kept::of(values::encode(&())?));
    let state = Arc::clone(&queue.state);
    queue
        .jobs
        .file()
        .write(0, move |tx| register(tx, &state, (id, now, next), &text, &value))
        .map_err(|error| queue.fail(None, error))?;
    queue.state.alarm.lower(next);
    Ok(())
}

fn register(
    c: &Connection,
    queue: &QueueState,
    (id, now, next): (i64, i64, i64),
    text: &str,
    value: &Kept,
) -> Result<()> {
    let key = queue.name.as_str();
    let Some(there) = rows::job_by_key(c, queue.id, key)? else {
        rows::check_room(c, queue.id, queue.policy.max_waiting)?;
        let job = NewJob { queue: queue.id, id, key: Some(key), at: next, repeat: Some(text), group: None, value };
        return rows::insert(c, &job);
    };
    if there.repeat.as_deref() == Some(text) {
        return Ok(());
    }
    let change = Change { next, repeat: Some(text), group: None, value };
    if rows::lease_held(c, there.id, now)? {
        return rows::ask_again(c, queue.id, &there, there.again, &change);
    }
    rows::renew(c, queue.id, key, &there, &change)
}
