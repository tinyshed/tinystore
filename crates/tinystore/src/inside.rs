//! kv, jobs and search inside an application's database: a bucket or a queue
//! opened from a database lives in its file, beside its tables, and commits
//! with its rows in `db.tx`, and a queue's job in `db.batch` too; a table's
//! index follows a table, changed in the commit that changes a row. This is
//! the one place two engines meet, so that neither imports the other, and so
//! the async API's database opens its own here.

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

#[cfg(feature = "jobs")]
impl<V> crate::jobs::Queue<V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    /// A job's add for a batch of the database the queue was opened from:
    /// in [`Database::batch`] it commits with the batch's rows or not at
    /// all, and shares the commit with every other write of the file, where
    /// [`Database::tx`] holds the writer and pays a sync of its own. It adds
    /// as [`add`](crate::jobs::Queue::add) does, unless its id is taken; a
    /// queue kept in another file fails the batch, `Invalid`.
    ///
    /// ```no_run
    /// # #[derive(serde::Serialize, serde::Deserialize)]
    /// # struct Index { note: i64 }
    /// # fn main() -> tinystore::Result<()> {
    /// # let store = tinystore::Store::open("data", Default::default())?;
    /// # let note = 7;
    /// let db = store.database("app").open()?;
    /// let indexing = db.queue::<Index>("indexing").open()?;
    /// db.batch([
    ///     tinystore::sql!("update notes set version = version + 1 where id = ?", note).into(),
    ///     indexing.adding(&Index { note }),
    /// ])?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn adding(&self, value: &V) -> crate::sql::Write {
        crate::jobs::JobCall::new(self, None).adding(value)
    }
}

#[cfg(feature = "jobs")]
impl<V> crate::jobs::JobCall<'_, V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    /// The job's add for a batch of the database the queue was opened from,
    /// with what the call asks beside its value; see
    /// [`Queue::adding`](crate::jobs::Queue::adding).
    pub fn adding(self, value: &V) -> crate::sql::Write {
        crate::sql::Write::lent(self.lent(crate::jobs::Change::Add, value))
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
        let read_rows: crate::search::ReadRows<T> = std::sync::Arc::new(crate::sql::rows_on::<T>);
        crate::search::TableIndexBuilder::in_file(std::sync::Arc::clone(self.shared()), name, read_rows)
    }

    /// Drops the table's index `name` this database keeps: its triggers come
    /// off the table in one commit and its rows go in the background; says
    /// whether there was one. A handle of it is refused from then on.
    pub fn drop_search_index(&self, name: &str) -> crate::Result<bool> {
        crate::search::drop_in_file(self.shared(), name)
    }
}

#[cfg(all(feature = "kv", feature = "async"))]
impl crate::aio::sql::Database {
    /// A bucket kept in this database's file, as [`Database::bucket`] opens
    /// one, its calls awaited: its writes commit with the rows in
    /// [`tx`](crate::aio::sql::Database::tx) through `tx.with(&bucket)`.
    pub fn bucket<V: crate::kv::Value>(&self, name: &str) -> crate::aio::kv::BucketBuilder<V> {
        crate::aio::kv::BucketBuilder::new(self.blocking().bucket(name), std::sync::Arc::clone(self.pool()))
    }
}

#[cfg(all(feature = "jobs", feature = "async"))]
impl crate::aio::sql::Database {
    /// A queue kept in this database's file, as [`Database::queue`] opens
    /// one, its calls awaited: its jobs commit with the rows in
    /// [`tx`](crate::aio::sql::Database::tx) through `tx.with(&queue)`, and
    /// in [`batch`](crate::aio::sql::Database::batch) through
    /// [`adding`](crate::aio::jobs::Queue::adding).
    pub fn queue<V>(&self, name: &str) -> crate::aio::jobs::QueueBuilder<V>
    where
        V: Serialize + DeserializeOwned + Send + 'static,
    {
        crate::aio::jobs::QueueBuilder::new(self.blocking().queue(name), std::sync::Arc::clone(self.pool()))
    }
}

#[cfg(all(feature = "jobs", feature = "async"))]
impl<V> crate::aio::jobs::Queue<V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    /// A job's add for a batch of the database the queue was opened from, as
    /// [`Queue::adding`](crate::jobs::Queue::adding) makes one: it checks the
    /// call and encodes its value, with no I/O, and the batch is awaited.
    pub fn adding(&self, value: &V) -> crate::sql::Write {
        self.blocking().adding(value)
    }
}

#[cfg(all(feature = "jobs", feature = "async"))]
impl<V> crate::aio::jobs::JobCall<'_, V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    /// The job's add for a batch of the database the queue was opened from,
    /// with what the call asks beside its value; see
    /// [`Queue::adding`](crate::jobs::Queue::adding).
    pub fn adding(self, value: &V) -> crate::sql::Write {
        self.into_blocking().adding(value)
    }
}

#[cfg(all(feature = "search", feature = "async"))]
impl crate::aio::sql::Database {
    /// An index of a table of this database, as [`Database::search_index`]
    /// opens one, its calls awaited. Its rows change with the table's, in
    /// their commit; a search inside [`tx`](crate::aio::sql::Database::tx)
    /// goes through `tx.with(&index)` and sees what the transaction wrote.
    pub fn search_index<T: DeserializeOwned + Send + 'static>(
        &self,
        name: &str,
    ) -> crate::aio::search::TableIndexBuilder<T> {
        self.blocking().search_index(name).into()
    }

    /// Drops the table's index `name` this database keeps, as
    /// [`Database::drop_search_index`] does. It runs in one transaction, so
    /// it waits in the database's line with its transactions, on no thread,
    /// and once polled it runs to its end: the triggers come off the table
    /// in one commit or not at all, whether or not its future is still awaited.
    pub fn drop_search_index(
        &self,
        name: &str,
    ) -> impl std::future::Future<Output = crate::Result<bool>> + Send + 'static {
        let name = name.to_owned();
        self.run_in_line(&format!("index {name}: its drop"), move |db| db.drop_search_index(&name))
    }
}

#[cfg(all(test, feature = "kv", feature = "jobs"))]
#[path = "inside_tests.rs"]
mod tests;
