//! The async API's handles of the KV engine: buckets, counters, rate limits,
//! quotas and once, the same names, builders and verbs as [`crate::kv`]'s,
//! each call that reads, writes or waits returning a future to `.await`.
//!
//! ```no_run
//! # async fn example() -> tinystore::Result<()> {
//! use std::time::Duration;
//! use tinystore::Options;
//! use tinystore::aio::Store;
//!
//! let store = Store::open("data", Options::default())?;
//! let codes = store.bucket::<i64>("sign-in-codes").ttl(Duration::from_mins(15)).open()?; // a plain call
//! codes.set("K7Q2", &42).await?;
//! assert_eq!(codes.take("K7Q2").await?, Some(42)); // read and delete in one write
//! assert_eq!(codes.take("K7Q2").await?, None); // a second click finds nothing
//! # Ok(())
//! # }
//! ```
//!
//! Opening a handle stays a plain call, made once at start, and so do
//! `under`, `key` and a key's options, which touch no file. A handle crosses
//! to the blocking API and back: `Bucket::from(bucket)` and
//! `bucket.blocking()` are the same bucket. The answers are the blocking
//! API's own types: [`Entry`](crate::kv::Entry), [`Page`](crate::kv::Page),
//! [`Version`](crate::kv::Version) and [`Allowance`](crate::kv::Allowance).
//!
//! # Where the work runs
//!
//! | A call        | Runs                                          | Its future dropped once polled        |
//! |---------------|-----------------------------------------------|---------------------------------------|
//! | a write       | queued at its file's writer in the first poll | commits all the same; its answer goes |
//! | a key's read  | first poll if a reader is idle, else the pool | answered then, or stops as a read     |
//! | a memory step | in the first poll, else on the pool           | applied then, or on the pool          |
//! | a page        | on the store's pool                           | stops, and its reader goes back       |
//! | a once's wait | a waiter in the engine's runs, on no thread   | leaves; nothing else changes          |
//! | passed on     | on a copy, on a thread of its own             | runs to its end                       |
//!
//! - A write is a bucket's `set`, `create`, `add`, `take`, `delete`,
//!   `expire` and `clear`, an add, delete or clear of counters written each
//!   add, and a quota's `allow`, `refund` and `reset`.
//! - A key's read is a bucket's `get`, `entry` and `has`, a counter's `get`,
//!   and a quota's or a rate limit's `peek`.
//! - A memory step is a rate limit's `allow` and `reset`, and an add or
//!   delete of counters opened with `flush_every`: a write that commits with
//!   a later flush, so that once polled it applies as a write commits.
//! - A page is a bucket's `list`, and each page of its `all`.
//! - A once's wait is a run's for the run of its key under way.
//! - Passed on, on a node that copies kv.db from its home, is every write
//!   and memory step, and a once run's claim of its key, which the home
//!   answers once the run under way there ends. An add of counters written
//!   each add is the exception: it is queued on the node and answered at
//!   once, on the store's pool (REPL-12).
//!
//! No call commits, syncs the disk or waits on the thread that polls it. A
//! read that would wait there, for a reader, for a flush of what memory
//! holds or for a renewal of an idle key, runs again on the store's pool. A
//! call passed on waits for its home's answer on a thread of its own, as the
//! blocking call waits on its caller's, so that a slow home holds none of
//! the pool's threads, which every other call shares.
//!
//! # Writes that share a commit
//!
//! A write is queued at its first poll and holds no thread while it waits,
//! so writes joined in one task may share a commit, each still its own
//! savepoint that fails alone: `tokio::try_join!(sessions.set(token, &s),
//! seen.add(event_id))`. Writes that must commit together, or that a read
//! decides, are a transaction: `store.tx(|tx| …)`, whose closure takes these
//! handles through `tx.with(&handle)` as it takes blocking ones.

mod bucket;
mod counters;
mod once;
mod quota;
mod rate_limit;

pub use bucket::{All, Bucket, BucketBuilder, KeyCall};
pub use counters::{Counters, CountersBuilder};
pub use once::{Once, OnceBuilder};
pub use quota::{Quota, QuotaBuilder};
pub use rate_limit::{RateLimit, RateLimitBuilder};

/// What an awaited call is called in its errors: `kv bucket sessions: key "42": a set`.
fn named(shown: String, call: &str) -> String {
    format!("{shown}: {call}")
}

#[cfg(all(test, feature = "replication"))]
#[path = "kv/copy_tests.rs"]
mod copy_tests;
