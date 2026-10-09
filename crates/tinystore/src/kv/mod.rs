//! The application's current state: values by key in buckets, branches a user
//! clears in one call, expiry on the store's clock, and versions for writes
//! that must not lose another's.
//!
//! ```no_run
//! # use std::time::Duration;
//! # fn main() -> tinystore::Result<()> {
//! let store = tinystore::Store::open("data", Default::default())?;
//! let codes = store.bucket::<i64>("login-codes").ttl(Duration::from_secs(15 * 60)).open()?;
//! codes.set("481-205", &42)?;
//! assert_eq!(codes.take("481-205")?, Some(42)); // a code is used once
//! # Ok(())
//! # }
//! ```

mod bucket;
mod cells;
mod engine;
mod path;
mod value;

pub use bucket::{All, Bucket, BucketBuilder, Entry, PAGE_KEYS, Page, Version, Write, expires_at, if_version, ttl};
pub use engine::Maintenance;
pub use path::Key;
pub use value::{Bytes, Value};

pub(crate) use bucket::Put;
pub(crate) use cells::Cell;
pub(crate) use value::Raw;

use crate::{Result, Store};

/// Deletes what expired and what large clears hid. The store runs it every
/// minute; a store opened without background work leaves it to the caller.
pub fn maintain(store: &Store) -> Result<Maintenance> {
    engine::Kv::of(store)?.maintain()
}
