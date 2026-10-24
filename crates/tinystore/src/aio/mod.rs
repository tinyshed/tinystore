//! The async API: the same store, files and engines as the blocking API, its
//! calls that wait returning a future to `.await`.
//!
//! `aio` is async I/O, as redis-rs names it. Its futures wake through
//! [`std::task::Waker`], so tokio, async-std, smol or an executor of the
//! program's own runs them: TinyStore brings no runtime, and nothing here
//! needs a `spawn`.
//!
//! ```no_run
//! # async fn example() -> tinystore::Result<()> {
//! use tinystore::Options;
//! use tinystore::aio::Store;
//!
//! let store = Store::open("data", Options::default())?; // opening stays a plain call
//! let stock = store.blocking().bucket::<i64>("stock").open()?;
//! let left = store.tx(move |tx| tx.with(&stock).get("sku-1")).await?; // the transaction is awaited
//! store.close().await?;
//! # Ok(())
//! # }
//! ```
//!
//! Built so far are the store and the KV engine's handles: [`Store`] opens,
//! crosses to and from the blocking [`crate::Store`], runs a transaction and
//! closes, and [`kv`]'s buckets, counters, rate limits, quotas and once are
//! awaited. Another engine's handle opened from [`Store::blocking`] is called
//! inside [`Store::tx`], or on a thread that may block, as
//! `tokio::task::spawn_blocking` gives one. Opening a store or a handle stays
//! a plain call, made once at start, since a first open may migrate a file.
//!
//! # One store, two APIs
//!
//! [`Store::from`] takes a blocking [`crate::Store`], and [`Store::blocking`]
//! gives it back: each is a cheap clone of the same store, its engines and
//! its files, never a second store. A command-line tool can call the blocking
//! API while a server beside it awaits, on one store.
//!
//! # Where the work runs
//!
//! | A call       | Runs                                          | Its future dropped once polled        |
//! |--------------|-----------------------------------------------|---------------------------------------|
//! | a write      | queued at its file's writer in the first poll | commits all the same; its answer goes |
//! | a point read | first poll if a reader is idle, else the pool | answered then, or stops as a read     |
//! | a page       | on the store's pool                           | stops, and its reader goes back       |
//! | a wait       | a waiter in the engine, on no thread          | leaves; nothing else changes          |
//! | `store.tx`   | its closure, blocking, on the store's pool    | runs to its end                       |
//! | `close`      | on a thread the call starts                   | closes all the same                   |
//!
//! A write is a key's `set` or `take`, a point read a key's `get` or a
//! counter, a page a bucket's `list`, and a wait a `once` run's for the run
//! of its key under way: each engine's module says which of its calls are
//! which.
//!
//! None commits, syncs the disk or waits on the thread that polls its
//! future, so an executor's thread never waits on the disk. The pool is the
//! store's own threads, at most sixteen, started as calls need them, each
//! ended after a minute without work and all of them stopped when the store
//! closes. A future does nothing before its first poll: one made and dropped
//! unpolled runs nothing.
//!
//! On a node that copies a file from its home, a write passed on to the home
//! waits for its answer on a thread of its own rather than the pool's, and so
//! does a `once` run's claim of its key there, which the home answers once
//! the run under way ends: a pool held by such waits would stall every other
//! call.
//!
//! # A transaction's closure is blocking
//!
//! [`Store::tx`] runs its closure on the store's pool, never on the
//! executor's thread: it holds its file's writer, so it must not wait for the
//! program's awaits. Inside, calls go through `tx.with(&handle)` and are
//! blocking, as in [`crate::Store::tx`]. A panic in the closure rolls the
//! transaction back and is resumed at the `.await`.
//!
//! # Errors and answers
//!
//! Every call answers as its blocking twin does, with the same
//! [`Error`](crate::Error) and [`ErrorKind`](crate::ErrorKind). A call made
//! once its store closed, or still waiting for the pool as it closed, fails
//! `Closed`. An awaited call from inside a transaction's closure, through a
//! `block_on`, fails `Invalid`, as a blocking call around a transaction does.
//!
//! # Closing
//!
//! `store.close().await` closes the store on a thread the call starts, which
//! waits for background work, the handlers running and every engine's last
//! writes. A store whose last handle is dropped closes on the dropping
//! thread, which may be the executor's: await `close` instead.

mod oneshot;
mod run;
mod store;

pub use store::Store;

#[cfg(feature = "backup")]
pub mod backup;
#[cfg(feature = "blobs")]
pub mod blobs;
#[cfg(feature = "channels")]
pub mod channels;
#[cfg(feature = "config")]
pub mod config;
#[cfg(feature = "jobs")]
pub mod jobs;
#[cfg(feature = "kv")]
pub mod kv;
#[cfg(feature = "locks")]
pub mod locks;
#[cfg(feature = "metrics")]
pub mod metrics;
#[cfg(feature = "records")]
pub mod records;
#[cfg(feature = "search")]
pub mod search;
#[cfg(feature = "sql")]
pub mod sql;
