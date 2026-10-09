//! What a queue's options say: how a job is retried, how long a run may take,
//! how many run and start, and how long what is done or failed is kept.

use std::time::Duration;

use crate::{Error, Result};

/// How many jobs run at once: in all, across every worker of the store, and
/// in each group. `Concurrency::total(8).group(2)` bounds both;
/// `Concurrency::default().group(2)` each group alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Concurrency {
    pub(crate) total: Option<u32>,
    pub(crate) group: Option<u32>,
}

impl Concurrency {
    pub fn total(jobs: u32) -> Concurrency {
        Concurrency { total: Some(jobs), group: None }
    }

    /// At most `jobs` of each group run at once, a group being what an add
    /// named; one customer's backlog cannot hold back the others.
    pub fn group(mut self, jobs: u32) -> Concurrency {
        self.group = Some(jobs);
        self
    }
}

impl From<u32> for Concurrency {
    fn from(jobs: u32) -> Concurrency {
        Concurrency::total(jobs)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Policy {
    pub(crate) attempts: u32,
    pub(crate) backoff: (Duration, Duration),
    pub(crate) timeout: Duration,
    pub(crate) concurrency: Concurrency,
    pub(crate) rate: Option<(u32, Duration)>,
    pub(crate) dedupe: Option<Duration>,
    pub(crate) keep: Duration,
    pub(crate) max_waiting: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            attempts: 10,
            backoff: (Duration::from_secs(1), Duration::from_hours(1)),
            timeout: Duration::from_mins(1),
            concurrency: Concurrency::default(),
            rate: None,
            dedupe: None,
            keep: Duration::from_hours(24 * 7),
            max_waiting: 10_000_000,
        }
    }
}

impl Policy {
    pub(crate) fn check(&self) -> Result<()> {
        let (initial, max) = self.backoff;
        let refused = if self.attempts == 0 {
            Some("attempts of 0: the first run is one".to_owned())
        } else if initial.is_zero() || max < initial {
            Some(format!("a backoff from {initial:?} to {max:?}"))
        } else if self.timeout < Duration::from_millis(1) {
            Some(format!("a timeout of {:?}", self.timeout))
        } else if self.concurrency.total == Some(0) || self.concurrency.group == Some(0) {
            Some("a concurrency of 0, which would run nothing".to_owned())
        } else if self.rate.is_some_and(|(count, per)| count == 0 || per < Duration::from_millis(1)) {
            Some("a rate of nothing a span".to_owned())
        } else if self.max_waiting == 0 {
            Some("a maxWaiting of 0, which would refuse every add".to_owned())
        } else {
            None
        };
        refused.map_or(Ok(()), |why| Err(Error::invalid(why)))
    }

    /// The handlers a worker runs at once unless it says: the queue's total,
    /// or one, which is the one order a queue promises.
    pub(crate) fn local(&self) -> u32 {
        self.concurrency.total.unwrap_or(1)
    }

    /// Whether the queue bounds what runs or starts, so that a worker claims no
    /// job ahead for a handler still busy: the jobs it holds are the ones running.
    pub(crate) fn bounded(&self) -> bool {
        self.concurrency.total.is_some() || self.concurrency.group.is_some() || self.rate.is_some()
    }
}
