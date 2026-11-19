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
use super::rows::{self, Change, NewJob, Own};
use super::run::{Outcome, Run};
use super::state::{Kind, QueueState};
use super::values::{self, Kept};
use super::work::Worker;
use crate::engine::Home;
use crate::{Error, ErrorKind, Result, Store};

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
        refuse_on_a_copy(&schedule.queue)?;
        schedule.worker = Some(schedule.queue.work(move |(): (), run: &Run| handler(run))?);
        Ok(schedule)
    }

    /// Opens the schedule without running it: for a program that reads where
    /// it is, or a test that runs it with `run_due`.
    pub fn open(self) -> Result<Schedule> {
        let describe = || format!("jobs schedule {}", self.name);
        let asked = self.repeat.ok_or_else(|| Error::invalid("a schedule needs every or cron").within(describe()))?;
        let repeat = asked.of(&self.name).map_err(|error| error.within(describe()))?;
        #[cfg(feature = "replication")]
        let copy = super::engine::Jobs::of(&self.store)?.file().is_copy();
        #[cfg(feature = "replication")]
        if copy {
            super::forward::pass_on_schedule(&self.store, &self.name, &asked)
                .map_err(|error| error.within(describe()))?;
        }
        let queue = Queue::<()>::open(&Home::Store(self.store.clone()), &self.name, Kind::Schedule, self.policy)?;
        #[cfg(feature = "replication")]
        if copy {
            return Ok(Schedule { queue, worker: None });
        }
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
    #[cfg(any(feature = "wire", feature = "async"))]
    pub(crate) fn queue(&self) -> &Queue<()> {
        &self.queue
    }

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

    /// Runs the schedule once as soon as a worker is free, under its own
    /// rules, and leaves its later times as they were: run at 03:29, a
    /// schedule of 03:30 runs again at 03:30. A run under way is followed by
    /// one more right after it, never beside it; a run waiting for a retry
    /// runs now, its attempts going on. A schedule whose job a cancel took is
    /// `not_found` until the program opens it again.
    pub fn run_now(&self) -> Result<()> {
        run_now(&self.queue)
    }
}

impl fmt::Debug for Schedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.queue.state.describe())
    }
}

/// Refuses a worker of a schedule kept in a copy of jobs' file, whose home's
/// workers run it.
pub(crate) fn refuse_on_a_copy(queue: &Queue<()>) -> Result<()> {
    #[cfg(feature = "replication")]
    let copy = queue.jobs.file().is_copy();
    #[cfg(not(feature = "replication"))]
    let copy = false;
    if copy {
        return Err(Error::new(
            crate::ErrorKind::Unavailable,
            "a copy runs no schedule: its home's workers run it, as its file is written there",
        )
        .within(queue.state.describe()));
    }
    Ok(())
}

/// Makes the schedule's one job repeat as the code says: added at its next
/// time when it is not there, given the code's repeat when it keeps another.
/// A job a run holds keeps its row, the new repeat taking over after the run.
pub(super) fn keep_repeat<V>(queue: &Queue<V>, repeat: &Repeat) -> Result<()>
where
    V: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
{
    let now = queue.jobs.now();
    let next = repeat.next(now).ok_or_else(|| Error::invalid("the repeat never runs again"))?;
    let (text, value) = (repeat.text(), Kept::of(values::encode(&())?));
    let state = Arc::clone(&queue.state);
    let due = queue
        .jobs
        .file()
        .write(0, move |tx| register(tx, &state, (now, next), &text, &value))
        .map_err(|error| queue.fail(None, error))?;
    queue.state.wake(due);
    Ok(())
}

/// Runs a schedule's one job now, as [`Schedule::run_now`] says, once its
/// write commits.
pub(crate) fn run_now<V>(queue: &Queue<V>) -> Result<()>
where
    V: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
{
    #[cfg(feature = "replication")]
    if queue.jobs.file().is_copy() {
        let store = queue.jobs.store().map_err(|error| queue.fail(None, error))?;
        return super::forward::pass_on_run_now(&store, &queue.state.name).map_err(|error| queue.fail(None, error));
    }
    let (now, state) = (queue.jobs.now(), Arc::clone(&queue.state));
    queue.jobs.file().write(0, move |tx| due_now(tx, &state, now)).map_err(|error| queue.fail(None, error))?;
    queue.state.wake(now);
    Ok(())
}

/// `run_now` without waiting: the write is queued, and `done` is called on
/// the thread that commits it. A copy passes it to its home on a thread of
/// its own.
#[cfg(any(feature = "wire", feature = "async"))]
pub(crate) fn run_now_then<V>(queue: &Queue<V>, done: impl FnOnce(Result<()>) + Send + 'static)
where
    V: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
{
    #[cfg(feature = "replication")]
    if queue.jobs.file().is_copy() {
        let queue = queue.clone();
        std::thread::spawn(move || done(run_now(&queue)));
        return;
    }
    let (now, state, named) = (queue.jobs.now(), Arc::clone(&queue.state), queue.state.describe());
    let writing = Arc::clone(&state);
    queue.jobs.file().submit(
        0,
        move |tx| due_now(tx, &writing, now),
        move |written| done(written.map(|()| state.wake(now)).map_err(|error| error.within(named))),
    );
}

/// Makes a schedule's job due now: one a run holds asks one run more right
/// after it, as a set of a running job does, so that the schedule never runs
/// beside itself (JOB-08); one waiting for its time or for a retry is due.
fn due_now(c: &Connection, queue: &QueueState, now: i64) -> Result<()> {
    let key = queue.name.as_str();
    let Some(there) = rows::job_by_key(c, queue.id, key)? else {
        let why = "the schedule has no job: a cancel took it, until the program opens the schedule again";
        return Err(Error::new(ErrorKind::NotFound, why));
    };
    if rows::lease_held(c, there.id, now)? {
        return rows::run_after(c, queue.id, &there, now);
    }
    if rows::due_of(there.next) > now {
        rows::due_now(c, queue.id, key, &there, now)?;
    }
    Ok(())
}

/// Keeps the schedule's job as the code's repeat says, and answers when it
/// is due.
fn register(c: &Connection, queue: &QueueState, (now, next): (i64, i64), text: &str, value: &Kept) -> Result<i64> {
    let key = queue.name.as_str();
    let Some(there) = rows::job_by_key(c, queue.id, key)? else {
        // a job failed for good holds the id, as a set finds it: the schedule starts again in its place
        rows::drop_failed(c, queue.id, key)?;
        rows::check_room(c, queue.id, queue.policy.max_waiting)?;
        let id = rows::next_id(c)?;
        let job = NewJob {
            queue: queue.id,
            id,
            key: Some(key),
            at: next,
            repeat: Some(text),
            group: None,
            error: None,
            own: Own::default(),
            value,
        };
        return rows::insert(c, &job).map(|()| next);
    };
    if there.repeat.as_deref() == Some(text) {
        return Ok(rows::due_of(there.next));
    }
    if rows::lease_held(c, there.id, now)? {
        let change = Change { next, repeat: Some(text), group: None, own: Own::default(), value };
        return rows::ask_again(c, queue.id, &there, there.again, &change).map(|()| next);
    }
    // a run missed while the program was down stays due, and runs once
    // before the new repeat takes over
    let due = if rows::due_of(there.next) <= now { there.next } else { next };
    let change = Change { next: due, repeat: Some(text), group: None, own: Own::default(), value };
    rows::renew(c, queue.id, key, &there, &change)?;
    Ok(due)
}
