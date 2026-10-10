//! TinyStore: an embedded data runtime on SQLite.
//!
//! One directory, held by one [`Store`] through its `LOCK`, a file per engine.
//! The store gives every engine the clock it reads, the memory budget its work
//! reserves from, the background work it runs and the errors they all share.

#[cfg(feature = "blobs")]
pub mod blobs;
mod clock;
mod durable;
#[cfg(feature = "kv")]
mod encryption;
pub mod engine;
mod error;
#[cfg(all(feature = "sql", any(feature = "kv", feature = "jobs")))]
mod inside;
#[cfg(feature = "jobs")]
pub mod jobs;
mod key;
#[cfg(feature = "kv")]
pub mod kv;
mod memory;
pub mod pipe;
mod schedule;
#[cfg(feature = "blobs")]
mod space;
#[cfg(feature = "sql")]
pub mod sql;
pub(crate) mod sqlite;
mod store;
mod transaction;
pub(crate) mod wire;

pub use clock::{Clock, SystemClock, TestClock, unix_millis, unix_nanos};
pub use error::{Constraint, ConstraintKind, Error, ErrorKind, Result};
pub use key::Key;
pub use memory::{Memory, Reservation};
pub use sqlite::Durability;
pub use store::{Options, Store};
pub use transaction::{Transaction, Tx, TxHandle};
