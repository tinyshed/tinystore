//! The async API's handles of the Jobs engine: the queues, schedules,
//! workers and runs of [`tinystore::jobs`](crate::jobs), each call that
//! waits awaited.
//!
//! ```no_run
//! # use std::time::Duration;
//! # #[derive(serde::Serialize, serde::Deserialize)]
//! # struct Email { to: String }
//! # async fn render(_: &Email) -> tinystore::Result<String> { Ok(String::new()) }
//! # async fn deliver(_: &Email, _: &str) -> tinystore::Result<()> { Ok(()) }
//! # async fn example() -> tinystore::Result<()> {
//! use tinystore::aio::Store;
//! use tinystore::aio::jobs::Run;
//!
//! async fn send(email: Email, run: Run) -> tinystore::Result<()> {
//!     let body: String = run.step("render", || render(&email)).await?; // kept across attempts
//!     deliver(&email, &body).await
//! }
//!
//! let store = Store::open("data", Default::default())?;
//! let emails = store.queue::<Email>("emails").open()?; // opening stays a plain call
//! emails.delay(Duration::from_secs(5)).add(&Email { to: "ada@example.com".into() }).await?;
//!
//! let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
//! let worker = tokio::spawn(emails.work(send).until(stopped)); // a future the program spawns
//! let _ = stop.send(()); // no job more: the handlers under way finish, and their jobs settle
//! worker.await.expect("the worker's task")?;
//! store.close().await?;
//! # Ok(())
//! # }
//! ```
//!
//! What the calls answer is the blocking module's data: a
//! [`Job`](crate::jobs::Job), a [`Page`](crate::jobs::Page), and the
//! [`Filter`](crate::jobs::Filter) and [`Outcome`](crate::jobs::Outcome)
//! they take stay in [`tinystore::jobs`](crate::jobs).
//!
//! # Where the work runs
//!
//! | A call                   | Runs                                               | Its future dropped once polled  |
//! |--------------------------|----------------------------------------------------|---------------------------------|
//! | `add`, `set`, `update`   | queued at jobs.db's writer; commit on the pool     | commits; its answer is let go   |
//! | `cancel`, `settle`       | queued at jobs.db's writer; commit on the pool     | commits; its answer is let go   |
//! | `claim`                  | its lease a write, a spilled value a point read    | commits; its lease runs out     |
//! | `get`, `list`            | on the store's pool                                | stops; its reader goes back     |
//! | `all`, `watch`: streams  | each read on the store's pool                      | stops; a read under way stops   |
//! | `work`, `run_due`        | handlers in the future; claims on a store thread   | [below](#a-worker-is-a-future)  |
//! | `run.step`'s kept answer | in the first poll when a reader is idle, else pool | answered in that poll, or stops |
//! | `run.step`'s keep        | queued at jobs.db's writer; commit on the pool     | commits                         |
//!
//! None of them commits, syncs the disk or waits on the thread that polls its
//! future. A claim dropped once its lease was written gives its job back when
//! the lease, the queue's timeout, runs out, the attempt counted, as a
//! blocking claim never settled does. The other calls, opening a queue or a
//! schedule among them, do no I/O or are made once at start, since a first
//! open may migrate jobs.db. Inside a transaction's closure, `tx.with(&queue)`
//! gives the blocking [`TxQueue`](crate::jobs::TxQueue)'s calls.
//!
//! # A worker is a future
//!
//! [`Queue::work`] returns a [`Worker`] at once and starts nothing: the
//! program spawns it, `tokio::spawn(queue.work(handler))`. Its first poll
//! starts the claim loop on a thread of the store's, which leases due jobs
//! and settles finished ones in one grouped write, as a blocking worker's
//! does. The handlers run inside the worker's future, on the program's
//! executor, as many at once as the worker's concurrency.
//!
//! - [`Worker::until`] stops it gracefully: once its signal is ready the
//!   worker takes no job more, waits for the handlers under way, each within
//!   the queue's timeout and a grace, and returns once their answers are
//!   written. Jobs it claimed and had not started go back uncounted.
//! - Dropping it is the abort: the jobs claimed and not started go back
//!   uncounted, and those whose handler's future it dropped run again after
//!   their backoff, the attempt counted. The drop waits for nothing.
//! - The store's close stops it as `until` does, and it returns `Ok(())`.
//! - A cancel of a running job turns `run.stopped()` true, and what its
//!   handler returns settles nothing, as in the blocking API: the worker
//!   does not drop the handler's future.
//!
//! # Closing
//!
//! A store's close waits for the handlers its workers run, which only the
//! program's executor polls. `store.close().await` closes on a thread of its
//! own while the executor goes on polling them. A blocking close, or the drop
//! of the store's last handle, on the executor's only thread waits instead
//! for the queue's timeout and ten seconds more, and the jobs under way run
//! again once their leases end.

#[cfg(test)]
mod fixture;
mod queue;
mod run;
mod schedule;
mod watch;
mod worker;

use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;

pub use queue::{All, Claimed, JobCall, Queue, QueueBuilder};
pub use run::Run;
pub use schedule::{Schedule, ScheduleBuilder};
pub use watch::Watch;
pub use worker::{Worker, Workers};

use crate::pool::Pool;

impl super::Store {
    /// A queue of jobs whose values are `V`, as JSON, in the store's jobs.db,
    /// as the blocking [`Store::queue`](crate::Store::queue) opens one: the
    /// same builder, its calls awaited.
    pub fn queue<V>(&self, name: &str) -> QueueBuilder<V>
    where
        V: Serialize + DeserializeOwned + Send + 'static,
    {
        QueueBuilder::new(self.blocking().queue(name), Arc::clone(self.pool()))
    }

    /// A schedule: one repeating job under `name`, the code's own, as
    /// the blocking [`Store::schedule`](crate::Store::schedule) opens one.
    pub fn schedule(&self, name: &str) -> ScheduleBuilder {
        ScheduleBuilder::new(self.blocking().schedule(name), Arc::clone(self.pool()))
    }
}

/// The pool of the store a blocking queue was opened from, or one stopped
/// already when that store is gone, so that every call answers `Closed`.
fn pool_of<V>(queue: &crate::jobs::Queue<V>) -> Arc<Pool> {
    queue.jobs.store().map_or_else(|_| Pool::stopped(), |store| Pool::of_or_stopped(&store))
}
