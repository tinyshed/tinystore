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
mod maintain;
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
mod values;
mod work;
mod write;

pub use call::JobCall;
pub use maintain::{Maintenance, maintain};
pub use policy::Concurrency;
pub use queue::{All, Claimed, Queue, QueueBuilder, Workers};
pub use read::{Filter, Job, LastRun, PAGE_JOBS, Page, State};
pub use run::{Outcome, Run, When};
pub use schedule::{Schedule, ScheduleBuilder};
pub use work::Worker;

#[cfg(test)]
#[path = "queue_tests.rs"]
mod queue_tests;
#[cfg(test)]
#[path = "work_tests.rs"]
mod work_tests;
