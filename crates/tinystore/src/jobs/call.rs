use std::fmt;
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::queue::Queue;
use super::repeat::Asked as Repeat;
use super::values::{self, Kept};
use super::write::Prepared;
use crate::clock::millis;
use crate::{Error, Result, unix_millis};

/// An id or a group is at most this long.
const MAX_ID: usize = 1024;

/// The times a job may have: the years 1 to 9999, well before where parked
/// jobs begin.
const EARLIEST: i64 = -62_135_596_800_000;
const LATEST: i64 = 253_402_300_799_999;

/// A call on one job with what it asks beside its value, run by its last
/// step: `later.id(&key).at(nine_am).add(&draft)`.
#[must_use = "a job's call runs at its last step: add, set or update"]
pub struct JobCall<'a, V> {
    queue: &'a Queue<V>,
    asked: Asked,
}

/// What a call asks beside its value.
#[derive(Debug, Default)]
pub(crate) struct Asked {
    id: Option<String>,
    when: Option<When>,
    group: Option<String>,
    repeat: Option<Repeat>,
    /// A step the chain refused, which its last step reports.
    refused: Option<String>,
}

#[derive(Clone, Copy, Debug)]
enum When {
    At(SystemTime),
    After(Duration),
}

impl<'a, V> JobCall<'a, V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    pub(crate) fn new(queue: &'a Queue<V>) -> Self {
        JobCall { queue, asked: Asked::default() }
    }

    /// The job's id, 1 to 1024 bytes of text: what finds it again, and what
    /// adds it once however many times its caller asks.
    pub fn id(mut self, id: impl AsRef<str>) -> Self {
        self.asked.id = Some(id.as_ref().to_owned());
        self
    }

    /// Runs the job at `time`; a time in the past runs it now.
    pub fn at(self, time: SystemTime) -> Self {
        self.when(When::At(time))
    }

    /// Runs the job `span` from now.
    pub fn delay(self, span: Duration) -> Self {
        self.when(When::After(span))
    }

    /// The group whose running jobs the queue's concurrency bounds.
    pub fn group(mut self, group: impl AsRef<str>) -> Self {
        self.asked.group = Some(group.as_ref().to_owned());
        self
    }

    /// Repeats the job every `span`, each id at a phase of its own within it,
    /// the same across restarts; its first run within the next interval unless
    /// `at` or `delay` says. A repeat needs an id, which alone stops it.
    pub fn every(mut self, span: Duration) -> Self {
        self.asked.repeat = Some(Repeat::Every(span));
        self
    }

    /// Repeats the job by a five-field cron expression on the wall clock of
    /// `zone`, an IANA name, `"UTC"` among them.
    pub fn cron(mut self, expr: &str, zone: &str) -> Self {
        self.asked.repeat = Some(Repeat::Cron { expr: expr.to_owned(), zone: zone.to_owned() });
        self
    }

    /// Adds the job unless its id is taken: by a job scheduled, waiting,
    /// running or failed, or done while the queue's dedupe keeps it. Says
    /// whether it added one; a job without an id is always added.
    #[expect(clippy::should_implement_trait, reason = "a queue's add puts a job in it, as BullMQ's does: no sum")]
    pub fn add(self, value: &V) -> Result<bool> {
        self.queue.add_asked(self.asked, value)
    }

    /// Makes the id's job this value at this time, whatever it was: a job
    /// waiting is made new, a failed one starts again, and a running one runs
    /// once more after this run, with this value.
    pub fn set(self, value: &V) -> Result<()> {
        self.queue.set_asked(self.asked, value)
    }

    /// Changes the id's job when it has not started: its value, and its time,
    /// group or repeat when the call names them. Says whether it did: a job
    /// that runs, ran or never was answers `false`.
    pub fn update(self, value: &V) -> Result<bool> {
        self.queue.update_asked(self.asked, value)
    }

    fn when(mut self, when: When) -> Self {
        if self.asked.when.is_some() {
            self.asked.refused = Some("a time given twice: at or delay, not both".to_owned());
        }
        self.asked.when = Some(when);
        self
    }
}

impl<V> fmt::Debug for JobCall<'_, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobCall").field("asked", &self.asked).finish()
    }
}

impl Asked {
    pub(crate) fn with_id(id: &str) -> Asked {
        Asked { id: Some(id.to_owned()), ..Asked::default() }
    }

    pub(crate) fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Checks what was asked and gathers what the write needs, the value
    /// encoded, before the write waits for the writer.
    pub(crate) fn prepare<V: Serialize>(self, value: &V, id: i64, now: i64) -> Result<Prepared> {
        if let Some(refused) = self.refused {
            return Err(Error::invalid(refused));
        }
        for (what, text) in [("an id", &self.id), ("a group", &self.group)] {
            if let Some(text) = text
                && (text.is_empty() || text.len() > MAX_ID)
            {
                return Err(Error::invalid(format!("{what} of {} bytes, not 1 to {MAX_ID}", text.len())));
            }
        }
        let repeat = match (&self.repeat, &self.id) {
            (Some(_), None) => return Err(Error::invalid("a repeat without an id, which alone could stop it")),
            (Some(repeat), Some(id)) => Some(repeat.of(id)?),
            (None, _) => None,
        };
        let at = match self.when {
            Some(When::At(time)) => unix_millis(time),
            Some(When::After(span)) => now.saturating_add(millis(span)),
            None => repeat.as_ref().and_then(|repeat| repeat.next(now)).unwrap_or(now),
        };
        if !(EARLIEST..=LATEST).contains(&at) {
            return Err(Error::invalid("a time outside the years 1 to 9999"));
        }
        Ok(Prepared {
            id,
            key: self.id,
            at,
            timed: self.when.is_some(),
            repeat: repeat.map(|repeat| repeat.text()),
            group: self.group,
            value: Kept::of(values::encode(value)?),
            now,
        })
    }
}
