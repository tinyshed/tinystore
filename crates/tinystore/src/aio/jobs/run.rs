use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::SystemTime;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::queue::Queue;
use crate::jobs::{self, Lease, Outcome, When};
use crate::pool::Pool;
use crate::{Error, Result};

/// This call on a job, as [`jobs::Run`]: when it was due, which attempt it
/// is, whether it was told to stop, and the answers a handler returns. A
/// handler takes it by value, so that its future may move to another thread.
pub struct Run {
    pub(super) run: jobs::Run,
    pub(super) pool: Arc<Pool>,
}

impl Run {
    /// The run of a job `queue` leased.
    pub(super) fn of<V>(queue: &Queue<V>, lease: Arc<Lease>) -> Run {
        let (state, engine) = (Arc::clone(&queue.queue.state), Arc::clone(&queue.queue.jobs));
        Run { run: jobs::Run::new(lease, state, engine), pool: Arc::clone(&queue.pool) }
    }

    /// The id the job was added under; none for a job added without one.
    pub fn id(&self) -> Option<&str> {
        self.run.id()
    }

    /// When the run was due, which is earlier than now when it started late.
    pub fn at(&self) -> SystemTime {
        self.run.at()
    }

    /// This run's number among the job's attempts, the first being 1.
    pub fn attempt(&self) -> u32 {
        self.run.attempt()
    }

    pub fn group(&self) -> Option<&str> {
        self.run.group()
    }

    /// Whether the run was told to stop: a cancel took its job, or it ran past
    /// its queue's timeout. A handler that checks it between pieces of work
    /// stops in time; what it did before stays done.
    pub fn stopped(&self) -> bool {
        self.run.stopped()
    }

    /// Shows how far the run got to `get` and `watch`: any JSON up to 4 KiB. It
    /// lives in memory, so it is a plain call.
    pub fn set_progress(&self, progress: &impl Serialize) -> Result<()> {
        self.run.set_progress(progress)
    }

    /// Runs `work` once for the job under `name` and keeps what it answered,
    /// as [`jobs::Run::step`] does: the attempt after a retry, a lost worker
    /// or a restart gets the kept answer back without running `work` again.
    ///
    /// The kept answer is read in the first poll when one of jobs.db's
    /// readers is idle, else on the store's pool, and `work`'s future runs
    /// inside this one. Dropped before `work` answers, nothing is kept and the
    /// step runs again in the next attempt; dropped once its answer's keep was
    /// queued at the writer, the keep commits.
    pub async fn step<T, E, F, W>(&self, name: &str, work: F) -> Result<T, E>
    where
        T: Serialize + DeserializeOwned,
        E: From<Error>,
        F: FnOnce() -> W,
        W: Future<Output = Result<T, E>>,
    {
        let describe = || format!("{}: step {name:?}", self.run.describe());
        if self.run.lease.cancelled() {
            return Err(jobs::cancelled_step().within(describe()).into());
        }
        if let Some(kept) = self.kept(name).await.map_err(|error| error.within(describe()))? {
            return jobs::answer_of(&kept).map_err(|error| error.within(describe()).into());
        }
        let answer = work().await?;
        let encoded = jobs::encoded(&answer).map_err(|error| error.within(describe()))?;
        self.keep(name, encoded).await.map_err(|error| error.within(describe()))?;
        Ok(answer)
    }

    /// The answer a step kept, a point read.
    fn kept(&self, name: &str) -> impl Future<Output = Result<Option<Vec<u8>>>> + Send + 'static {
        let (engine, lease, name) = (Arc::clone(&self.run.jobs), Arc::clone(&self.run.lease), name.to_owned());
        crate::aio::run::point(&self.pool, "its kept answer".to_owned(), move || jobs::kept(&engine, &lease, &name))
    }

    /// Keeps a step's answer at jobs.db's writer, while this attempt's lease
    /// holds the job.
    fn keep(&self, name: &str, encoded: Vec<u8>) -> impl Future<Output = Result<()>> + Send + 'static {
        let (engine, lease, name) = (Arc::clone(&self.run.jobs), Arc::clone(&self.run.lease), name.to_owned());
        crate::aio::run::write(&self.pool, "its keep".to_owned(), move |answer| {
            jobs::keep_then(&engine, &lease, &name, encoded, move |kept| answer.send(kept));
        })
    }

    /// The job's value, read after its claim committed: at once when its row
    /// holds it, else a point read of the row of its own it spilled to.
    pub(super) fn value<V>(&self) -> impl Future<Output = Result<V>> + Send + 'static
    where
        V: DeserializeOwned + Send + 'static,
    {
        let (engine, lease) = (Arc::clone(&self.run.jobs), Arc::clone(&self.run.lease));
        let what = "a claimed job's value".to_owned();
        let read = lease.spill.is_some().then(|| {
            let (engine, lease) = (Arc::clone(&engine), Arc::clone(&lease));
            crate::aio::run::point(&self.pool, what, move || jobs::read_value::<V>(&engine, &lease))
        });
        async move {
            match read {
                Some(read) => read.await,
                None => jobs::read_value::<V>(&engine, &lease),
            }
        }
    }

    /// Says a handler began the job; false when a cancel took it first.
    pub(super) fn begin(&self) -> bool {
        if !self.run.lease.begin(self.run.jobs.now()) {
            return false;
        }
        self.run.queue.watchers.changed();
        true
    }

    /// The answer that says the job is done, for a handler whose other
    /// branches answer `retry` or `fail`.
    #[must_use = "an answer does nothing unless the handler returns it"]
    pub fn done(&self) -> Outcome {
        self.run.done()
    }

    /// Runs the job again at `when`, the run counted as an attempt; past the
    /// queue's attempts it fails for good.
    #[must_use = "an answer does nothing unless the handler returns it"]
    pub fn retry(&self, when: impl Into<When>) -> Outcome {
        self.run.retry(when)
    }

    /// Runs the job again at `when`, the run not counted.
    #[must_use = "an answer does nothing unless the handler returns it"]
    pub fn snooze(&self, when: impl Into<When>) -> Outcome {
        self.run.snooze(when)
    }

    /// Fails the job for good now, `reason` kept as its error.
    #[must_use = "an answer does nothing unless the handler returns it"]
    pub fn fail(&self, reason: impl fmt::Display) -> Outcome {
        self.run.fail(reason)
    }
}

impl fmt::Debug for Run {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.run, f)
    }
}
