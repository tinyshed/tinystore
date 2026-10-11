//! TinyStore: an embedded data runtime on SQLite.
//!
//! One directory, held by one [`Store`] through its `LOCK`, a file per engine.
//! The store gives every engine the clock it reads, the memory budget its work
//! reserves from, the background work it runs and the errors they all share.

#[cfg(feature = "blobs")]
pub mod blobs;
mod clock;
#[cfg(feature = "console")]
pub mod console;
#[cfg(feature = "store")]
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
#[cfg(feature = "store")]
mod memory;
// the protocol's session answers kv in every call it runs, so the wire needs kv
#[cfg(feature = "kv")]
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
#[cfg(feature = "store")]
mod transaction;
#[cfg(feature = "kv")]
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
#[cfg(feature = "store")]
pub use transaction::{Transaction, Tx, TxHandle};
