//! Work an application must do later, or now but outside the request that
//! asked for it: queues of jobs in the order of their time in jobs.db, a
//! handler run on each as it falls due, retries, repeats, and ids that find a
//! job again.
//!
//! ```no_run
//! # use std::time::Duration;
//! # #[derive(serde::Serialize, serde::Deserialize)]
//! # struct Reminder { user: i64, text: String }
//! # fn push(_: i64, _: &str) -> std::io::Result<()> { Ok(()) }
//! # fn main() -> tinystore::Result<()> {
//! let store = tinystore::Store::open("data", Default::default())?;
//! let reminders = store.queue::<Reminder>("reminders").open()?;
//! reminders.delay(Duration::from_hours(1)).add(&Reminder { user: 42, text: "Call mom".into() })?;
//! let worker = reminders.work(|reminder: Reminder, _run| push(reminder.user, &reminder.text))?;
//! # Ok(())
//! # }
//! ```
//!
//! A job runs at least once: a process that dies after the work and before
//! the job was marked done runs it again, so a handler keys its effect by what
//! the job names.

mod alarm;
mod call;
mod claim;
mod cron;
mod engine;
#[cfg(test)]
mod fixture;
#[cfg(feature = "replication")]
pub(crate) mod forward;
mod maintain;
mod measure;
mod policy;
mod queue;
mod rate;
mod read;
mod repeat;
mod rows;
mod run;
mod schedule;
mod state;
mod steps;
mod tx;
mod values;
mod watch;
mod work;
mod write;

pub use call::JobCall;
pub use maintain::{Maintenance, maintain};
pub use policy::Concurrency;
pub use queue::{All, Claimed, Queue, QueueBuilder, Workers};
pub use read::{Filter, Job, LastRun, PAGE_JOBS, Page, State};
pub use run::{Outcome, Run, When};
pub use schedule::{Schedule, ScheduleBuilder};
pub use tx::TxQueue;
pub use watch::Watch;
pub use work::Worker;

// What the wire and the async API take of jobs: a worker whose handlers run
// elsewhere, a client's or a future's, its watches, its steps.
#[cfg(any(feature = "wire", feature = "async"))]
pub(crate) use {
    claim::{How, Lease},
    steps::kept,
    watch::{Report, Seen, Subscription},
    work::{Answers, Ended, Hand, Remote, RemoteWork, read_value, start_remote},
};

#[cfg(feature = "wire")]
pub(crate) use {steps::keep_encoded, watch::Hears};

// What the async API alone takes: a claim, a settlement and a step's keep
// that answer through a completion.
#[cfg(feature = "async")]
pub(crate) use {
    queue::{Settling, Took},
    schedule::refuse_on_a_copy,
    steps::{answer_of, cancelled_step, encoded, keep_then},
    work::{answered, value_or_how},
};

// What a job's call writes: the wire's calls, the async API's, and a
// database's batch.
#[cfg(any(feature = "wire", feature = "sql", feature = "async"))]
pub(crate) use queue::Change;

// What the command line lists of a store's names.
#[cfg(feature = "client")]
pub(crate) use engine::queues;

#[cfg(test)]
#[path = "queue_tests.rs"]
mod queue_tests;
#[cfg(test)]
#[path = "watch_tests.rs"]
mod watch_tests;
#[cfg(test)]
#[path = "work_tests.rs"]
mod work_tests;
