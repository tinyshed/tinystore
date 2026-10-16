//! TinyStore: an embedded data runtime on SQLite.
//!
//! One directory, held by one [`Store`] through its `LOCK`, a file per engine.
//! The store gives every engine the clock it reads, the memory budget its work
//! reserves from, the background work it runs and the errors they all share.

#[cfg(feature = "backup")]
pub mod backup;
#[cfg(feature = "blobs")]
pub mod blobs;
#[cfg(feature = "channels")]
pub mod channels;
mod clock;
#[cfg(any(feature = "records", feature = "metrics", feature = "backup"))]
mod compress;
#[cfg(feature = "console")]
pub mod console;
#[cfg(any(feature = "kv", feature = "blobs", feature = "backup"))]
mod durable;
#[cfg(feature = "kv")]
mod encryption;
#[cfg(feature = "store")]
pub mod engine;
mod error;
#[cfg(all(feature = "sql", any(feature = "kv", feature = "jobs")))]
mod inside;
#[cfg(feature = "jobs")]
pub mod jobs;
mod key;
#[cfg(feature = "kv")]
pub mod kv;
#[cfg(all(feature = "records", feature = "config"))]
mod log_config;
#[cfg(feature = "config")]
pub use kv::config;
#[cfg(feature = "locks")]
pub use kv::locks;
#[cfg(feature = "store")]
mod memory;
#[cfg(feature = "metrics")]
pub mod metrics;
#[cfg(feature = "wire")]
pub mod pipe;
#[cfg(feature = "records")]
pub mod records;
#[cfg(feature = "store")]
mod schedule;
#[cfg(feature = "blobs")]
mod space;
#[cfg(feature = "sql")]
pub mod sql;
#[cfg(feature = "store")]
pub(crate) mod sqlite;
#[cfg(feature = "store")]
mod store;
#[cfg(any(feature = "kv", feature = "jobs", feature = "sql"))]
mod transaction;
#[cfg(feature = "typos")]
pub mod typos;
#[cfg(feature = "wire")]
pub(crate) mod wire;

pub use clock::{Clock, SystemClock, TestClock, unix_millis, unix_nanos};
pub use error::{Constraint, ConstraintKind, Error, ErrorKind, Result};
pub use key::Key;
#[cfg(feature = "store")]
pub use memory::{Memory, Reservation};
#[cfg(feature = "store")]
pub use sqlite::Durability;
#[cfg(feature = "store")]
pub use store::{Options, Store};
#[cfg(any(feature = "kv", feature = "jobs"))]
pub use transaction::Tx;
#[cfg(any(feature = "kv", feature = "jobs", feature = "sql"))]
pub use transaction::{Transaction, TxHandle};
