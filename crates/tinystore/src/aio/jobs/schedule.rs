use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use super::queue::Queue;
use super::run::Run;
use super::worker::Worker;
use crate::Result;
use crate::jobs::{self, Job, Outcome};
use crate::pool::Pool;

/// A schedule being opened, as [`jobs::ScheduleBuilder`]: its name and its
/// repeat.
#[must_use = "a schedule opens with work() or open()"]
pub struct ScheduleBuilder {
    builder: jobs::ScheduleBuilder,
    pool: Arc<Pool>,
}

impl fmt::Debug for ScheduleBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.builder, f)
    }
}

impl ScheduleBuilder {
    pub(super) fn new(builder: jobs::ScheduleBuilder, pool: Arc<Pool>) -> Self {
        ScheduleBuilder { builder, pool }
    }

    fn with(self, option: impl FnOnce(jobs::ScheduleBuilder) -> jobs::ScheduleBuilder) -> Self {
        ScheduleBuilder { builder: option(self.builder), pool: self.pool }
    }

    /// Runs every `span`, at the schedule's own phase within it.
    pub fn every(self, span: Duration) -> Self {
        self.with(|builder| builder.every(span))
    }

    /// Runs by a five-field cron expression on the wall clock of `zone`, an
    /// IANA name, `"UTC"` among them.
    pub fn cron(self, expr: &str, zone: &str) -> Self {
        self.with(|builder| builder.cron(expr, zone))
    }

    /// Runs a run gets before it fails, the first counted: 10. A failed run
    /// retries, never past the next time, and the schedule goes on.
    pub fn attempts(self, attempts: u32) -> Self {
        self.with(|builder| builder.attempts(attempts))
    }

    pub fn backoff(self, initial: Duration, max: Duration) -> Self {
        self.with(|builder| builder.backoff(initial, max))
    }

    pub fn timeout(self, timeout: Duration) -> Self {
        self.with(|builder| builder.timeout(timeout))
    }

    /// Opens the schedule without running it, a plain call made once at
    /// start: it writes the code's repeat, as [`jobs::ScheduleBuilder::open`].
    pub fn open(self) -> Result<Schedule> {
        Ok(Schedule::of(self.builder.open()?, self.pool))
    }

    /// Opens the schedule and gives the worker that runs `handler` at each
    /// time, for a program that never reads the schedule: the program spawns
    /// it, as [`Schedule::work`]'s. A copy of jobs.db runs no schedule.
    pub fn work<F, W, R, E>(self, handler: F) -> Result<Worker>
    where
        F: Fn(Run) -> W + Send + Sync + 'static,
        W: Future<Output = std::result::Result<R, E>> + Send + 'static,
        R: Into<Outcome> + 'static,
        E: fmt::Display + 'static,
    {
        let schedule = self.open()?;
        jobs::refuse_on_a_copy(&schedule.queue.queue)?;
        Ok(schedule.work(handler))
    }
}

/// A repeat the code owns, as [`jobs::Schedule`]: one repeating job under the
/// schedule's name. It holds no worker: the program spawns the one
/// [`Schedule::work`] gives, which stops by [`Worker::until`] or a drop.
pub struct Schedule {
    schedule: jobs::Schedule,
    queue: Queue<()>,
}

impl From<jobs::Schedule> for Schedule {
    /// The same schedule, its calls awaited, with the worker it was opened
    /// with, if any, still running.
    fn from(schedule: jobs::Schedule) -> Schedule {
        let pool = super::pool_of(schedule.queue());
        Schedule::of(schedule, pool)
    }
}

impl Schedule {
    fn of(schedule: jobs::Schedule, pool: Arc<Pool>) -> Schedule {
        let queue = Queue { queue: schedule.queue().clone(), pool };
        Schedule { schedule, queue }
    }

    /// The same schedule, for its blocking calls.
    pub fn blocking(&self) -> &jobs::Schedule {
        &self.schedule
    }

    fn name(&self) -> &str {
        &self.queue.queue.state.name
    }

    /// When it runs next and how its last run went, read on the store's pool.
    pub fn get(&self) -> impl Future<Output = Result<Option<Job<()>>>> + Send + 'static {
        self.queue.get(self.name())
    }

    /// The worker that runs `handler` at each time, as a queue's worker does
    /// and gets the run alone: it starts at its first poll. On a copy of
    /// jobs.db it ends there with `unavailable`, since the home's workers run
    /// the schedule.
    pub fn work<F, W, R, E>(&self, handler: F) -> Worker
    where
        F: Fn(Run) -> W + Send + Sync + 'static,
        W: Future<Output = std::result::Result<R, E>> + Send + 'static,
        R: Into<Outcome> + 'static,
        E: fmt::Display + 'static,
    {
        match jobs::refuse_on_a_copy(&self.queue.queue) {
            Ok(()) => self.queue.work(move |(): (), run: Run| handler(run)),
            Err(refused) => Worker::refused(&self.queue, refused),
        }
    }

    /// Runs what is due, as [`Queue::run_due`] does, and answers how many runs
    /// ran.
    pub fn run_due<F, W, R, E>(&self, handler: F) -> impl Future<Output = Result<usize>> + Send + 'static
    where
        F: Fn(Run) -> W + Send + Sync + 'static,
        W: Future<Output = std::result::Result<R, E>> + Send + 'static,
        R: Into<Outcome> + 'static,
        E: fmt::Display + 'static,
    {
        self.queue.run_due(move |(): (), run: Run| handler(run))
    }

    /// Removes the schedule's job, until the program opens it again; a write
    /// queued at jobs.db's writer at the first poll.
    pub fn cancel(&self) -> impl Future<Output = Result<bool>> + Send + 'static {
        self.queue.cancel(self.name())
    }
}

impl fmt::Debug for Schedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.schedule, f)
    }
}
