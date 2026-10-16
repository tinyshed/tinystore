//! kv, jobs and search inside an application's database: a bucket or a queue
//! opened from a database lives in its file, beside its tables, and commits
//! with its rows in `db.tx`; a table's index follows a table, changed in the
//! commit that changes a row. This is the one place two engines meet, so
//! that neither imports the other.

#[cfg(feature = "jobs")]
use serde::Serialize;
#[cfg(any(feature = "jobs", feature = "search"))]
use serde::de::DeserializeOwned;

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

#[cfg(feature = "search")]
impl Database {
    /// An index of a table of this database, kept in its file: the table's
    /// rows found again by their words, their vectors or both, as
    /// [`Store::index`](crate::Store::index) finds documents, each hit's value
    /// its row as [`Database::all`] reads one. The table owns the rows and the
    /// index follows them: every change of a row, by any statement, trigger
    /// or migration, reaches the index in the commit that makes it or not at
    /// all, and one by a process beside the store at the store's next write
    /// or within a second.
    ///
    /// ```no_run
    /// # #[derive(serde::Deserialize)]
    /// # struct Article { id: i64, title: String, body: String }
    /// # fn main() -> tinystore::Result<()> {
    /// # let store = tinystore::Store::open("data", Default::default())?;
    /// let db = store.database("app").migrations("migrations").open()?;
    /// let articles =
    ///     db.search_index::<Article>("articles_search").table("articles").field("title", 2.0).field("body", 1.0).open()?;
    /// let found = articles.search("новый заголовок").run()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn search_index<T: DeserializeOwned + 'static>(&self, name: &str) -> crate::search::TableIndexBuilder<T> {
        let read_row: crate::search::ReadRow<T> = std::sync::Arc::new(read_row::<T>);
        crate::search::TableIndexBuilder::in_file(std::sync::Arc::clone(self.shared()), name, read_row)
    }

    /// Drops the table's index `name` this database keeps: its triggers come
    /// off the table in one commit and its rows go in the background; says
    /// whether there was one. A handle of it is refused from then on.
    pub fn drop_search_index(&self, name: &str) -> crate::Result<bool> {
        crate::search::drop_in_file(self.shared(), name)
    }
}

/// The row a table's index reads for a hit, as a `T`, as the database's own
/// reads read one.
#[cfg(feature = "search")]
fn read_row<T: DeserializeOwned>(
    connection: &rusqlite::Connection,
    select: &str,
    key: &rusqlite::types::Value,
) -> crate::Result<Option<T>> {
    let statement = crate::sql::Sql::new(select);
    let statement = match key {
        rusqlite::types::Value::Integer(number) => statement.bind(number),
        rusqlite::types::Value::Text(text) => statement.bind(text),
        _ => return Err(crate::Error::internal("a row's key of neither an integer nor text")),
    };
    crate::sql::row_on(connection, &statement)
}

#[cfg(all(test, feature = "kv", feature = "jobs"))]
#[path = "inside_tests.rs"]
mod tests;
