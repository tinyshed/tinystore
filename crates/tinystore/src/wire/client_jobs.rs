//! A shell's writes to a queue: `jobs add` and `jobs cancel`.

use std::io::{Read, Write};
use std::time::{Duration, SystemTime};

use super::{Client, millis_of};
use crate::Result;
use crate::wire::protocol::{self as wire, method};

/// A job as `jobs add` adds it.
#[derive(Clone, Debug, Default)]
pub struct NewJob {
    /// JSON's text.
    pub value: String,
    /// What names it in its queue: a queue that holds it adds nothing.
    pub id: Option<String>,
    pub delay: Option<Duration>,
    pub at: Option<SystemTime>,
}

impl<S: Read + Write> Client<S> {
    /// The handle of the queue `name`, in the database `database` when given,
    /// with the queue's defaults: a job keeps none of them, its worker's
    /// own deciding how it runs.
    pub fn queue(&mut self, name: &str, database: Option<u64>) -> Result<u64> {
        let open = wire::JobsQueueOpen { name: name.to_owned(), database, ..wire::JobsQueueOpen::default() };
        self.handle(method::JOBS_QUEUE_OPEN, &open)
    }

    /// Adds the job: whether it was added, which a job of its id the queue
    /// holds already is not.
    pub fn add(&mut self, queue: u64, job: &NewJob) -> Result<bool> {
        let call = wire::JobsCall {
            handle: queue,
            id: job.id.clone(),
            value: job.value.clone(),
            at: job.at.map(crate::unix_millis),
            delay: job.delay.map(millis_of),
            ..wire::JobsCall::default()
        };
        Ok(self.answer::<wire::JobsChanged>(method::JOBS_ADD, &call)?.changed)
    }

    /// Cancels the job `id`: whether the queue held one to cancel.
    pub fn cancel(&mut self, queue: u64, id: &str) -> Result<bool> {
        let id = wire::JobsId { handle: queue, id: id.to_owned() };
        Ok(self.answer::<wire::JobsChanged>(method::JOBS_CANCEL, &id)?.changed)
    }
}
