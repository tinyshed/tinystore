//! A queue's calls inside a transaction of its file: a job added there exists
//! only if the transaction commits, and the queue's workers wake once it has.

use std::fmt;
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::call::{Asked, JobCall};
use super::queue::Queue;
use super::read::{self, Job};
use crate::transaction::sealed::Sealed;
use crate::{Result, Transaction, TxHandle};

impl<V> Sealed for Queue<V> {}

impl<V> TxHandle for Queue<V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    type InTx<'t>
        = TxQueue<'t, V>
    where
        Self: 't;

    fn in_tx<'t>(&'t self, tx: &'t Transaction<'t>) -> TxQueue<'t, V> {
        TxQueue { queue: self, tx }
    }
}

/// A queue's calls inside a transaction, `tx.with(&queue)`: its writes and
/// its reads of one job. A worker, a watch and a page read outside it.
pub struct TxQueue<'a, V> {
    queue: &'a Queue<V>,
    tx: &'a Transaction<'a>,
}

impl<'a, V> TxQueue<'a, V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    /// Adds a job, as [`Queue::add`] does, once the transaction commits.
    pub fn add(&self, value: &V) -> Result<bool> {
        self.queue.add_asked(Asked::default(), value, Some(self.tx))
    }

    pub fn set(&self, id: impl AsRef<str>, value: &V) -> Result<()> {
        self.queue.set_asked(Asked::with_id(id.as_ref()), value, Some(self.tx))
    }

    pub fn update(&self, id: impl AsRef<str>, value: &V) -> Result<bool> {
        self.queue.update_asked(Asked::with_id(id.as_ref()), value, Some(self.tx))
    }

    /// Takes the job under `id`, as [`Queue::cancel`] does; a running job's
    /// handler is told to stop once the transaction commits.
    pub fn cancel(&self, id: impl AsRef<str>) -> Result<bool> {
        self.queue.cancel_in(id.as_ref(), Some(self.tx))
    }

    /// The job under `id` as the transaction sees it, its own writes among it.
    pub fn get(&self, id: impl AsRef<str>) -> Result<Option<Job<V>>> {
        let (id, now) = (id.as_ref(), self.queue.jobs.now());
        let state = &self.queue.state;
        self.queue.run_in(self.tx, |c| read::get(c, state, id, now)).map_err(|error| self.queue.fail(Some(id), error))
    }

    pub fn id(&self, id: impl AsRef<str>) -> JobCall<'a, V> {
        JobCall::new(self.queue, Some(self.tx)).id(id)
    }

    pub fn at(&self, time: SystemTime) -> JobCall<'a, V> {
        JobCall::new(self.queue, Some(self.tx)).at(time)
    }

    pub fn delay(&self, span: Duration) -> JobCall<'a, V> {
        JobCall::new(self.queue, Some(self.tx)).delay(span)
    }

    pub fn group(&self, group: impl AsRef<str>) -> JobCall<'a, V> {
        JobCall::new(self.queue, Some(self.tx)).group(group)
    }
}

impl<V> fmt::Debug for TxQueue<'_, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TxQueue({:?})", self.queue)
    }
}
