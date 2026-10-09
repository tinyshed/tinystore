//! What a handler gets beside a job's value: the run, this call on the job,
//! and the answers it may return.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::claim::{How, Lease};
use super::engine::Jobs;
use super::state::QueueState;
use super::steps;
use crate::clock::{from_unix_millis, millis};
use crate::{Error, Result, unix_millis};

/// A run's progress, as JSON, is at most this long: it lives in memory.
const MAX_PROGRESS: usize = 4 << 10;

/// This call on a job: when it was due, which attempt it is, whether it was
/// told to stop, and the answers a handler returns to say what happens next.
pub struct Run {
    pub(crate) lease: Arc<Lease>,
    pub(crate) queue: Arc<QueueState>,
    pub(crate) jobs: Arc<Jobs>,
    deadline: Instant,
}

impl Run {
    pub(crate) fn new(lease: Arc<Lease>, queue: Arc<QueueState>, jobs: Arc<Jobs>) -> Run {
        let deadline = Instant::now() + queue.policy.timeout;
        Run { lease, queue, jobs, deadline }
    }

    /// The id the job was added under; none for a job added without one.
    pub fn id(&self) -> Option<&str> {
        self.lease.key.as_deref()
    }

    /// When the run was due: "thirty days ago" is counted from it even when the
    /// run starts late.
    pub fn at(&self) -> SystemTime {
        from_unix_millis(self.lease.at)
    }

    /// This run's number among the job's attempts, the first being 1.
    pub fn attempt(&self) -> u32 {
        u32::try_from(self.lease.attempt).unwrap_or(u32::MAX)
    }

    pub fn group(&self) -> Option<&str> {
        self.lease.group.as_deref()
    }

    /// Whether the run was told to stop: a cancel took its job, or it ran past
    /// its queue's timeout. A handler that checks it between pieces of work
    /// stops in time; what it did before stays done.
    pub fn stopped(&self) -> bool {
        self.lease.cancelled() || Instant::now() >= self.deadline
    }

    /// Shows how far the run got to `get` and `watch`: any JSON up to 4 KiB, a
    /// fraction or a count as the application likes. It lives in memory, and a
    /// run a restart ends starts again from nothing.
    pub fn set_progress(&self, progress: &impl Serialize) -> Result<()> {
        let value = serde_json::to_value(progress)
            .map_err(|error| Error::invalid("a progress JSON cannot write").with_source(error))?;
        let size = serde_json::to_vec(&value).map_or(0, |bytes| bytes.len());
        if size > MAX_PROGRESS {
            return Err(
                Error::limit(format!("a progress of {size} bytes, past {MAX_PROGRESS}")).within(self.describe())
            );
        }
        self.lease.set_progress(value);
        self.queue.watchers.changed();
        Ok(())
    }

    /// Runs `work` once for the job under `name` and keeps what it answered:
    /// the attempt after a retry, a lost worker or a restart gets the kept
    /// answer back without running `work` again. A step whose attempt ends
    /// before its answer is kept runs again, so what `work` does outside the
    /// store should bear doing twice.
    pub fn step<T, E>(&self, name: &str, work: impl FnOnce() -> Result<T, E>) -> Result<T, E>
    where
        T: Serialize + DeserializeOwned,
        E: From<Error>,
    {
        steps::run(self, name, work)
    }

    /// The answer of a handler whose job is done, for a handler that answers
    /// otherwise elsewhere; one that never does returns `Ok(())`.
    #[must_use = "an answer does nothing unless the handler returns it"]
    pub fn done(&self) -> Outcome {
        Outcome(Answer::Done)
    }

    /// Runs the job again at `when`, the run counted as an attempt; past the
    /// queue's attempts it fails for good.
    #[must_use = "an answer does nothing unless the handler returns it"]
    pub fn retry(&self, when: impl Into<When>) -> Outcome {
        Outcome(Answer::Retry(when.into()))
    }

    /// Runs the job again at `when`, the run not counted: a provider that asks
    /// for a minute, a row that says later.
    #[must_use = "an answer does nothing unless the handler returns it"]
    pub fn snooze(&self, when: impl Into<When>) -> Outcome {
        Outcome(Answer::Snooze(when.into()))
    }

    /// Fails the job for good now, `reason` kept as its error.
    #[must_use = "an answer does nothing unless the handler returns it"]
    pub fn fail(&self, reason: impl fmt::Display) -> Outcome {
        Outcome(Answer::Fail(reason.to_string()))
    }

    pub(crate) fn describe(&self) -> String {
        match &self.lease.key {
            Some(key) => format!("{}: id {key:?}", self.queue.describe()),
            None => self.queue.describe(),
        }
    }
}

impl fmt::Debug for Run {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Run")
            .field("queue", &self.queue.name)
            .field("id", &self.lease.key)
            .field("attempt", &self.lease.attempt)
            .finish()
    }
}

/// When a retry or a snooze runs the job again: a span from now, or a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    After(Duration),
    At(SystemTime),
}

impl From<Duration> for When {
    fn from(span: Duration) -> When {
        When::After(span)
    }
}

impl From<SystemTime> for When {
    fn from(time: SystemTime) -> When {
        When::At(time)
    }
}

impl When {
    fn at(self, now: i64) -> i64 {
        match self {
            When::After(span) => now.saturating_add(millis(span)),
            When::At(time) => unix_millis(time),
        }
    }
}

/// What a handler answers about its job. `Ok(())` is done; the run's `retry`,
/// `snooze` and `fail` make the others.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome(Answer);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Answer {
    Done,
    Retry(When),
    Snooze(When),
    Fail(String),
}

impl From<()> for Outcome {
    fn from((): ()) -> Outcome {
        Outcome(Answer::Done)
    }
}

impl Outcome {
    pub(crate) fn fail_with(cause: String) -> Outcome {
        Outcome(Answer::Fail(cause))
    }

    /// The settlement this answer asks for at `now`.
    pub(crate) fn how(self, now: i64) -> How {
        match self.0 {
            Answer::Done => How::Done,
            Answer::Retry(when) => {
                How::Retry { at: Some(when.at(now)), cause: "the handler asked to retry".to_owned() }
            }
            Answer::Snooze(when) => How::Snooze { at: when.at(now) },
            Answer::Fail(cause) => How::Fail { cause },
        }
    }
}
