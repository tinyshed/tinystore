//! kv and jobs inside an application's database: a bucket or a queue opened
//! from a database lives in its file, beside its tables, and commits with its
//! rows in `db.tx`. This is the one place two engines meet, so that neither
//! imports the other.

#[cfg(feature = "jobs")]
use serde::{Serialize, de::DeserializeOwned};

use crate::sql::Database;

#[cfg(feature = "kv")]
impl Database {
    /// A bucket of values by key kept in this database's file, as
    /// [`Store::bucket`](crate::Store::bucket) keeps one in kv.db: the same
    /// handle with the same calls, whose writes commit with the rows in
    /// [`Database::tx`] through `tx.with(&bucket)`.
    pub fn bucket<V: crate::kv::Value>(&self, name: &str) -> crate::kv::BucketBuilder<V> {
        crate::kv::BucketBuilder::in_file(std::sync::Arc::clone(self.shared()), name)
    }
}

#[cfg(feature = "jobs")]
impl Database {
    /// A queue of jobs kept in this database's file, as
    /// [`Store::queue`](crate::Store::queue) keeps one in jobs.db: the same
    /// handle with the same calls and workers, whose jobs commit with the rows
    /// in [`Database::tx`] through `tx.with(&queue)`. It shares the database's
    /// writer, which is the point and its cost.
    pub fn queue<V>(&self, name: &str) -> crate::jobs::QueueBuilder<V>
    where
        V: Serialize + DeserializeOwned + Send + 'static,
    {
        crate::jobs::QueueBuilder::in_file(std::sync::Arc::clone(self.shared()), name)
    }
}

#[cfg(all(test, feature = "kv", feature = "jobs"))]
#[path = "inside_tests.rs"]
mod tests;
