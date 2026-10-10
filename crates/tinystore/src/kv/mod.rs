//! The application's current state: values by key in buckets, branches a user
//! clears in one call, expiry on the store's clock, versions for writes that
//! must not lose another's, and counters, rate limits, quotas and answers kept
//! once a key beside them.
//!
//! ```no_run
//! # use std::time::Duration;
//! # fn main() -> tinystore::Result<()> {
//! let store = tinystore::Store::open("data", Default::default())?;
//! let codes = store.bucket::<i64>("login-codes").ttl(Duration::from_mins(15)).open()?;
//! codes.set("481-205", &42)?;
//! assert_eq!(codes.take("481-205")?, Some(42)); // a code is used once
//! # Ok(())
//! # }
//! ```

mod allowance;
mod batch;
mod bucket;
mod buffer;
mod cells;
mod counters;
mod engine;
#[cfg(test)]
mod fixture;
mod key_call;
mod once;
mod path;
mod quota;
mod rate_limit;
mod renewals;
mod scope;
mod tx;
mod value;

pub use allowance::{Allowance, Window};
pub use bucket::{All, Bucket, BucketBuilder, Entry, PAGE_KEYS, Page, Version};
pub use counters::{Counters, CountersBuilder};
pub use engine::Maintenance;
pub use key_call::KeyCall;
pub use once::{Once, OnceBuilder};
pub use path::Key;
pub use quota::{Quota, QuotaBuilder};
pub use rate_limit::{RateLimit, RateLimitBuilder};
pub use tx::{Tx, TxBucket, TxCounters};
pub use value::{Bytes, Value};

pub(crate) use batch::{Batch, Outcome, Place};
pub(crate) use bucket::{Put, Stamp, WriteOptions};
pub(crate) use cells::Cell;
pub(crate) use once::{Hand, Handed, hand, rows as once_rows};
pub(crate) use path::MAX_PATH as MAX_KEY;
// The wire's calls of a bucket inside a database's transaction.
pub(crate) use value::Raw;
#[cfg(feature = "sql")]
pub(crate) use {bucket::clear_work, tx::call_on_branch, tx::call_on_key};

use crate::{Result, Store};

/// Writes what kv keeps in memory, then deletes what expired and what large
/// clears hid. The store runs it every minute, and writes memory every second;
/// a store opened without background work leaves both to the caller.
pub fn maintain(store: &Store) -> Result<Maintenance> {
    engine::Kv::of(store)?.maintain()
}
