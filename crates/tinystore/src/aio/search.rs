//! The async API's handles of the Search engine: the same indexes, builders
//! and answers as [`crate::search`], each call that reads, writes or waits
//! returning a future to `.await`.
//!
//! ```no_run
//! # async fn example() -> tinystore::Result<()> {
//! use tinystore::Options;
//! use tinystore::aio::Store;
//! use tinystore::aio::search::eq;
//!
//! #[derive(serde::Serialize, serde::Deserialize)]
//! struct Article {
//!     title: String,
//!     body: String,
//!     author_id: i64,
//! }
//!
//! let store = Store::open("data", Options::default())?;
//! let articles = store.index::<Article>("articles").field("title", 2.0).filters(["author_id"]).open()?;
//! let bread = Article { title: "Хлеб на закваске".into(), body: "…".into(), author_id: 42 };
//! articles.set("a-17", &bread).await?;
//! let found = articles.search("хлеба").filter(eq("author_id", &42)).run().await?;
//! let gone = articles.delete("a-17").await?; // true: there was one
//! # Ok(())
//! # }
//! ```
//!
//! Opening an index stays a plain call, as opening every handle does. The
//! answers are the blocking API's own types, re-exported here: [`Found`],
//! [`Hit`], [`Status`], [`Listed`], and the filters [`eq`], [`is_in`],
//! [`range`], [`missing`], [`or`] and [`and`].
//!
//! | A call                | Runs                                                | Its future dropped once polled  |
//! |-----------------------|-----------------------------------------------------|---------------------------------|
//! | a delete, a small set | a submit in the first poll, its commit on the pool  | commits; its answer is let go   |
//! | a set past 4 KiB      | its words and its commit on the store's pool        | commits; its answer is let go   |
//! | `get`                 | in the first poll when a reader is idle, else pool  | answered in that poll, or stops |
//! | a search, `status`    | on the store's pool                                 | stops; its reader goes back     |
//! | `list`                | on the store's pool                                 | stops; its reader goes back     |
//! | `ready`               | a waiter the index wakes, each look on the pool     | stops waiting, not the rebuild  |
//! | `store.drop_index`    | on the store's pool                                 | runs to its end                 |
//!
//! A set's JSON is made when its call is, and its words in its first poll,
//! where it takes its turn, or on the store's pool where its documents come
//! to more than 4 KiB of JSON, a `set_many`'s all together: the next search
//! sees it once its future is ready. A wait for the index to be ready looks
//! at its status as it is woken and every tenth of a second, on the pool.
//! A search's text, limits, filters and vector are checked when its
//! call is, so that one refused for them answers in its first poll and never
//! reaches the pool. A search inside a database's transaction stays
//! blocking: `tx.with(&index)` takes the async `TableIndex` and gives the
//! blocking `TxTableIndex`.

use std::fmt;
use std::future::Future;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::Store;
use super::oneshot::pending;
use super::run;
use crate::cancel;
use crate::pool::Pool;
use crate::search::Encoded;
use crate::{Key, Result};

#[cfg(feature = "sql")]
pub use crate::search::TxTableIndex;
pub use crate::search::{Condition, Document, Found, Hit, Listed, Rebuild, Status, and, eq, is_in, missing, or, range};

impl Store {
    /// The index `name`, made the first time it opens, as
    /// [`crate::Store::index`] opens one: its builder and its `open` are plain
    /// calls, and the index's calls are awaited.
    pub fn index<T: Serialize + DeserializeOwned>(&self, name: &str) -> IndexBuilder<'_, T> {
        IndexBuilder { builder: self.blocking().index(name), pool: self.pool() }
    }

    /// Drops the index `name` and every document it holds, as
    /// [`crate::Store::drop_index`] does, on the store's pool: the drop
    /// waits for the sets in flight, and once polled it runs to its end.
    pub fn drop_index(&self, name: &str) -> impl Future<Output = Result<bool>> + Send + 'static {
        let (store, what) = (self.blocking().clone(), format!("index {name}: its drop"));
        let name = name.to_owned();
        run::blocking(self.pool(), None, what, move || store.drop_index(&name))
    }
}

/// Every index the store's `search.db` holds, by name, as
/// [`crate::search::list`] reads them, on the store's pool.
pub fn list(store: &Store) -> impl Future<Output = Result<Vec<Listed>>> + Send + 'static {
    let blocking = store.blocking().clone();
    run::read(store.pool(), format!("{blocking:?}: its indexes"), move || crate::search::list(&blocking))
}

/// What [`Store::index`] opens: an index's settings, given where it opens.
#[must_use = "an index opens at `open`"]
pub struct IndexBuilder<'s, T> {
    builder: crate::search::IndexBuilder<'s, T>,
    pool: &'s Arc<Pool>,
}

impl<T> fmt::Debug for IndexBuilder<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.builder, f)
    }
}

impl<'s, T: Serialize + DeserializeOwned> IndexBuilder<'s, T> {
    /// As [`crate::search::IndexBuilder::field`].
    pub fn field(self, path: &str, weight: f64) -> Self {
        self.changed(|builder| builder.field(path, weight))
    }

    /// As [`crate::search::IndexBuilder::fields`].
    pub fn fields<S: AsRef<str>>(self, paths: impl IntoIterator<Item = S>) -> Self {
        self.changed(|builder| builder.fields(paths))
    }

    /// As [`crate::search::IndexBuilder::filters`].
    pub fn filters<S: AsRef<str>>(self, paths: impl IntoIterator<Item = S>) -> Self {
        self.changed(|builder| builder.filters(paths))
    }

    /// As [`crate::search::IndexBuilder::typo_tolerance`].
    pub fn typo_tolerance(self, on: bool) -> Self {
        self.changed(|builder| builder.typo_tolerance(on))
    }

    /// As [`crate::search::IndexBuilder::vectors`].
    pub fn vectors(self, dimensions: usize) -> Self {
        self.changed(|builder| builder.vectors(dimensions))
    }

    /// As [`crate::search::IndexBuilder::recall`].
    pub fn recall(self, recall: f64) -> Self {
        self.changed(|builder| builder.recall(recall))
    }

    /// As [`crate::search::IndexBuilder::vector_weight`].
    pub fn vector_weight(self, weight: f64) -> Self {
        self.changed(|builder| builder.vector_weight(weight))
    }

    /// As [`crate::search::IndexBuilder::rerank`].
    pub fn rerank(self) -> Self {
        self.changed(crate::search::IndexBuilder::rerank)
    }

    /// As [`crate::search::IndexBuilder::rebuild`].
    pub fn rebuild(self, rebuild: Rebuild) -> Self {
        self.changed(|builder| builder.rebuild(rebuild))
    }

    /// Opens the index as [`crate::search::IndexBuilder::open`] does: a plain
    /// call, made once at start, which never waits for the work a change of
    /// its settings needs.
    pub fn open(self) -> Result<Index<T>> {
        Ok(Index { index: self.builder.open()?, pool: Arc::clone(self.pool) })
    }

    /// Opens the index as [`open`](Self::open) does, and reads it again
    /// beside itself in the background, as
    /// [`crate::search::IndexBuilder::reindex`] does.
    pub fn reindex(self) -> Result<Index<T>> {
        Ok(Index { index: self.builder.reindex()?, pool: Arc::clone(self.pool) })
    }

    fn changed(
        self,
        change: impl FnOnce(crate::search::IndexBuilder<'s, T>) -> crate::search::IndexBuilder<'s, T>,
    ) -> Self {
        IndexBuilder { builder: change(self.builder), pool: self.pool }
    }
}

/// Documents of type `T` by id in a named index, found again by their words,
/// their vectors or both, its calls awaited: the same index as
/// [`crate::search::Index`], and the same again through [`From`] and
/// [`blocking`](Self::blocking).
pub struct Index<T> {
    index: crate::search::Index<T>,
    pool: Arc<Pool>,
}

impl<T> Clone for Index<T> {
    fn clone(&self) -> Self {
        Index { index: self.index.clone(), pool: Arc::clone(&self.pool) }
    }
}

impl<T> fmt::Debug for Index<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.index, f)
    }
}

impl<T> From<crate::search::Index<T>> for Index<T> {
    /// The same index, its calls awaited: a clone of its handle, never a
    /// second one.
    fn from(index: crate::search::Index<T>) -> Index<T> {
        let pool = pool_of(index.store());
        Index { index, pool }
    }
}

impl<T> Index<T> {
    /// The same index, for its blocking calls.
    pub fn blocking(&self) -> &crate::search::Index<T> {
        &self.index
    }
}

impl<T: DeserializeOwned + Send + 'static> Index<T> {
    pub fn name(&self) -> &str {
        self.index.name()
    }

    /// Where the index stands, as [`crate::search::Index::status`] reads it,
    /// on the store's pool.
    pub fn status(&self) -> impl Future<Output = Result<Status>> + Send + 'static {
        let index = self.index.clone();
        run::read(&self.pool, format!("index {}: its status", self.name()), move || index.status())
    }

    /// Waits until the index serves the settings it was opened with, as
    /// [`crate::search::Index::ready`] does, holding no thread: woken as its
    /// rebuild switches in or fails, and looking at its status again on the
    /// store's pool every tenth of a second. Dropped, it stops waiting, and
    /// the rebuild goes on. On a store opened without background work it
    /// runs the maintenance itself, a pass at a time on the store's pool,
    /// and a pass it began runs to its end.
    pub fn ready(&self) -> impl Future<Output = Result<()>> + Send + 'static {
        ready(self.index.clone(), Arc::clone(&self.pool))
    }

    /// A search by `text`'s words, as [`crate::search::Index::search`] makes
    /// one, run by its awaited [`run`](SearchCall::run).
    pub fn search(&self, text: &str) -> SearchCall<'_, T> {
        SearchCall { call: self.index.search(text), pool: &self.pool }
    }

    /// A search by `vector` alone, as [`crate::search::Index::search_vector`]
    /// makes one.
    pub fn search_vector<'i>(&'i self, vector: &'i [f32]) -> SearchCall<'i, T> {
        SearchCall { call: self.index.search_vector(vector), pool: &self.pool }
    }
}

impl<T: Serialize + DeserializeOwned + Send + 'static> Index<T> {
    /// The document `id`, or none, as [`crate::search::Index::get`] reads it:
    /// a point read, tried in the future's first poll.
    pub fn get(&self, id: impl Key) -> impl Future<Output = Result<Option<T>>> + Send + 'static {
        let (index, id) = (self.index.clone(), id.text().into_owned());
        run::point(&self.pool, format!("index {}: a get", self.name()), move || index.get(&id))
    }

    /// Sets the document `id` to `value`, replacing it whole, as
    /// [`crate::search::Index::set`] does; the next search sees it once the
    /// future is ready. On an index with vectors a set gives its vector, or
    /// says it has none, through [`id`](Self::id).
    pub fn set(&self, id: impl Key, value: &T) -> impl Future<Output = Result<()>> + Send + 'static {
        self.set_encoded(Encoded::new(id.text().into_owned(), value, None))
    }

    /// A set of the document `id` with its vector, run by its awaited last
    /// step: `products.id("p-3").vector(&embedding).set(&product).await`.
    pub fn id(&self, id: impl Key) -> DocumentCall<'_, T> {
        DocumentCall { index: self, id: id.text().into_owned(), vector: None }
    }

    /// Sets each document to its value, all of them in one commit or none,
    /// as [`crate::search::Index::set_many`] does.
    pub fn set_many<'v>(
        &self,
        documents: impl IntoIterator<Item = impl Document<'v, T>>,
    ) -> impl Future<Output = Result<()>> + Send + 'static
    where
        T: 'v,
    {
        let documents: Vec<Encoded> = documents
            .into_iter()
            .map(|document| {
                let (id, value, vector) = document.parts();
                Encoded::new(id, value, vector)
            })
            .collect();
        let index = self.index.clone();
        run::write(&self.pool, format!("index {}: a set_many", self.name()), move |answer| {
            index.set_many_then(documents, move |set| answer.send(set));
        })
    }

    /// Deletes the document `id`, as [`crate::search::Index::delete`] does;
    /// says whether there was one.
    pub fn delete(&self, id: impl Key) -> impl Future<Output = Result<bool>> + Send + 'static {
        let (index, id) = (self.index.clone(), id.text().into_owned());
        run::write(&self.pool, format!("index {}: a delete", self.name()), move |answer| {
            index.delete_then(id, move |deleted| answer.send(deleted));
        })
    }

    fn set_encoded(&self, document: Encoded) -> impl Future<Output = Result<()>> + Send + 'static {
        let index = self.index.clone();
        run::write(&self.pool, format!("index {}: a set", self.name()), move |answer| {
            index.set_then(document, move |set| answer.send(set));
        })
    }
}

/// A set of one document with its vector, run by [`DocumentCall::set`].
#[must_use = "a document is set at `set`"]
pub struct DocumentCall<'a, T> {
    index: &'a Index<T>,
    id: String,
    vector: Option<Option<&'a [f32]>>,
}

impl<T> fmt::Debug for DocumentCall<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DocumentCall").field("id", &self.id).finish_non_exhaustive()
    }
}

impl<'a, T: Serialize + DeserializeOwned + Send + 'static> DocumentCall<'a, T> {
    /// As [`crate::search::DocumentCall::vector`].
    pub fn vector(mut self, vector: &'a [f32]) -> Self {
        self.vector = Some(Some(vector));
        self
    }

    /// As [`crate::search::DocumentCall::without_vector`].
    pub fn without_vector(mut self) -> Self {
        self.vector = Some(None);
        self
    }

    /// Sets the document to `value`, its words and its vector, as
    /// [`crate::search::DocumentCall::set`] does.
    pub fn set(self, value: &T) -> impl Future<Output = Result<()>> + Send + 'static {
        self.index.set_encoded(Encoded::new(self.id, value, self.vector))
    }
}

/// A search of an index, run by its awaited [`SearchCall::run`].
#[must_use = "a search runs at `run`"]
pub struct SearchCall<'i, T> {
    call: crate::search::SearchCall<'i, T>,
    pool: &'i Arc<Pool>,
}

impl<T> fmt::Debug for SearchCall<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.call, f)
    }
}

impl<'i, T: DeserializeOwned + Send + 'static> SearchCall<'i, T> {
    /// As [`crate::search::SearchCall::vector`].
    pub fn vector(self, vector: &'i [f32]) -> Self {
        self.changed(|call| call.vector(vector))
    }

    /// As [`crate::search::SearchCall::vector_weight`].
    pub fn vector_weight(self, weight: f64) -> Self {
        self.changed(|call| call.vector_weight(weight))
    }

    /// As [`crate::search::SearchCall::rerank`].
    pub fn rerank(self) -> Self {
        self.changed(crate::search::SearchCall::rerank)
    }

    /// As [`crate::search::SearchCall::filter`].
    pub fn filter(self, condition: Condition) -> Self {
        self.changed(|call| call.filter(condition))
    }

    /// As [`crate::search::SearchCall::limit`].
    pub fn limit(self, limit: usize) -> Self {
        self.changed(|call| call.limit(limit))
    }

    /// As [`crate::search::SearchCall::offset`].
    pub fn offset(self, offset: usize) -> Self {
        self.changed(|call| call.offset(offset))
    }

    /// Runs the search on the store's pool, as
    /// [`crate::search::SearchCall::run`] does, each hit's value decoded
    /// there. Its text, its limits, its filters and its vector are checked
    /// here, so that a search refused for them answers in its first poll and
    /// never reaches the pool.
    pub fn run(self) -> impl Future<Output = Result<Found<T>>> + Send + 'static {
        let pool = self.pool;
        let read = self.call.asked().map(|(index, query)| {
            let (index, what) = (index.clone(), format!("index {}: a search", index.name()));
            run::read(pool, what, move || index.run_query(query))
        });
        async move { read?.await }
    }

    fn changed(
        self,
        change: impl FnOnce(crate::search::SearchCall<'i, T>) -> crate::search::SearchCall<'i, T>,
    ) -> Self {
        SearchCall { call: change(self.call), pool: self.pool }
    }
}

/// What `db.search_index` opens: a table's index and its settings, as
/// [`crate::search::TableIndexBuilder`] gives them, opened into an index
/// whose calls are awaited.
#[cfg(feature = "sql")]
#[must_use = "an index opens at `open`"]
pub struct TableIndexBuilder<T> {
    builder: crate::search::TableIndexBuilder<T>,
}

#[cfg(feature = "sql")]
impl<T> fmt::Debug for TableIndexBuilder<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.builder, f)
    }
}

#[cfg(feature = "sql")]
impl<T> From<crate::search::TableIndexBuilder<T>> for TableIndexBuilder<T> {
    /// The same settings, opened into an index whose calls are awaited.
    fn from(builder: crate::search::TableIndexBuilder<T>) -> TableIndexBuilder<T> {
        TableIndexBuilder { builder }
    }
}

#[cfg(feature = "sql")]
impl<T: DeserializeOwned + Send + 'static> TableIndexBuilder<T> {
    /// As [`crate::search::TableIndexBuilder::table`].
    pub fn table(self, table: &str) -> Self {
        self.changed(|builder| builder.table(table))
    }

    /// As [`crate::search::TableIndexBuilder::key`].
    pub fn key(self, column: &str) -> Self {
        self.changed(|builder| builder.key(column))
    }

    /// As [`crate::search::TableIndexBuilder::field`].
    pub fn field(self, column: &str, weight: f64) -> Self {
        self.changed(|builder| builder.field(column, weight))
    }

    /// As [`crate::search::TableIndexBuilder::fields`].
    pub fn fields<S: AsRef<str>>(self, columns: impl IntoIterator<Item = S>) -> Self {
        self.changed(|builder| builder.fields(columns))
    }

    /// As [`crate::search::TableIndexBuilder::filters`].
    pub fn filters<S: AsRef<str>>(self, columns: impl IntoIterator<Item = S>) -> Self {
        self.changed(|builder| builder.filters(columns))
    }

    /// As [`crate::search::TableIndexBuilder::typo_tolerance`].
    pub fn typo_tolerance(self, on: bool) -> Self {
        self.changed(|builder| builder.typo_tolerance(on))
    }

    /// As [`crate::search::TableIndexBuilder::vectors`].
    pub fn vectors(self, column: &str, dimensions: usize) -> Self {
        self.changed(|builder| builder.vectors(column, dimensions))
    }

    /// As [`crate::search::TableIndexBuilder::recall`].
    pub fn recall(self, recall: f64) -> Self {
        self.changed(|builder| builder.recall(recall))
    }

    /// As [`crate::search::TableIndexBuilder::vector_weight`].
    pub fn vector_weight(self, weight: f64) -> Self {
        self.changed(|builder| builder.vector_weight(weight))
    }

    /// As [`crate::search::TableIndexBuilder::rerank`].
    pub fn rerank(self) -> Self {
        self.changed(crate::search::TableIndexBuilder::rerank)
    }

    /// As [`crate::search::TableIndexBuilder::rebuild`].
    pub fn rebuild(self, rebuild: Rebuild) -> Self {
        self.changed(|builder| builder.rebuild(rebuild))
    }

    /// Opens the index as [`crate::search::TableIndexBuilder::open`] does: a
    /// plain call, made once at start, its table's rows read in the
    /// background, which [`TableIndex::ready`] waits for.
    pub fn open(self) -> Result<TableIndex<T>> {
        self.builder.open().map(TableIndex::from)
    }

    /// Opens the index as [`open`](Self::open) does, and reads it again
    /// beside itself in the background.
    pub fn reindex(self) -> Result<TableIndex<T>> {
        self.builder.reindex().map(TableIndex::from)
    }

    fn changed(
        self,
        change: impl FnOnce(crate::search::TableIndexBuilder<T>) -> crate::search::TableIndexBuilder<T>,
    ) -> Self {
        TableIndexBuilder { builder: change(self.builder) }
    }
}

/// The rows of a table found again by their words, their vectors or both, its
/// calls awaited: the same index as [`crate::search::TableIndex`], and the
/// same again through [`From`] and [`blocking`](Self::blocking). It has no
/// set or delete: its rows are the table's, and the index changes in their
/// commit.
#[cfg(feature = "sql")]
pub struct TableIndex<T> {
    index: crate::search::TableIndex<T>,
    pool: Arc<Pool>,
}

#[cfg(feature = "sql")]
impl<T> Clone for TableIndex<T> {
    fn clone(&self) -> Self {
        TableIndex { index: self.index.clone(), pool: Arc::clone(&self.pool) }
    }
}

#[cfg(feature = "sql")]
impl<T> fmt::Debug for TableIndex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.index, f)
    }
}

#[cfg(feature = "sql")]
impl<T: DeserializeOwned> From<crate::search::TableIndex<T>> for TableIndex<T> {
    /// The same index, its calls awaited: a clone of its handle, never a
    /// second one.
    fn from(index: crate::search::TableIndex<T>) -> TableIndex<T> {
        let pool = pool_of(index.inner().store());
        TableIndex { index, pool }
    }
}

#[cfg(feature = "sql")]
impl<T> TableIndex<T> {
    /// The same index, for its blocking calls.
    pub fn blocking(&self) -> &crate::search::TableIndex<T> {
        &self.index
    }
}

#[cfg(feature = "sql")]
impl<T: DeserializeOwned + Send + 'static> TableIndex<T> {
    pub fn name(&self) -> &str {
        self.index.name()
    }

    /// A search by `text`'s words, as [`crate::search::TableIndex::search`]
    /// makes one: each hit's id is its row's key and its value the row, read
    /// on the store's pool in the snapshot its search read.
    pub fn search(&self, text: &str) -> SearchCall<'_, T> {
        SearchCall { call: self.index.search(text), pool: &self.pool }
    }

    /// A search by `vector` alone, as
    /// [`crate::search::TableIndex::search_vector`] makes one.
    pub fn search_vector<'i>(&'i self, vector: &'i [f32]) -> SearchCall<'i, T> {
        SearchCall { call: self.index.search_vector(vector), pool: &self.pool }
    }

    /// Where the index stands, as [`crate::search::TableIndex::status`] reads
    /// it, on the store's pool.
    pub fn status(&self) -> impl Future<Output = Result<Status>> + Send + 'static {
        let index = self.index.clone();
        run::read(&self.pool, format!("index {}: its status", self.name()), move || index.status())
    }

    /// Waits until the index has read its table's rows and serves the
    /// settings it was opened with, as [`Index::ready`] waits: dropped, it
    /// stops waiting, and the table's rows are read on.
    pub fn ready(&self) -> impl Future<Output = Result<()>> + Send + 'static {
        ready(self.index.inner().clone(), Arc::clone(&self.pool))
    }
}

#[cfg(feature = "sql")]
impl<T> crate::transaction::sealed::Sealed for TableIndex<T> {}

/// A search inside a transaction of the table's database is blocking, on the
/// transaction's thread, as every call in a transaction is.
#[cfg(feature = "sql")]
impl<T: DeserializeOwned> crate::TxHandle for TableIndex<T> {
    type InTx<'h>
        = TxTableIndex<'h, T>
    where
        Self: 'h;

    fn in_tx<'h>(&'h self, tx: &'h crate::Transaction<'h>) -> TxTableIndex<'h, T> {
        crate::TxHandle::in_tx(&self.index, tx)
    }
}

/// The pool of the store an index opened in, or one stopped already where
/// the store is gone, whose calls answer `Closed`.
fn pool_of(store: Option<crate::Store>) -> Arc<Pool> {
    store.map_or_else(Pool::stopped, |store| Pool::of_or_stopped(&store))
}

/// An index's wait to be ready: a look at its status on the store's pool,
/// then, while it rebuilds, a wait for its next settle, or a pass of
/// maintenance on a store without background work, and a look again.
fn ready<T: DeserializeOwned + Send + 'static>(
    index: crate::search::Index<T>,
    pool: Arc<Pool>,
) -> impl Future<Output = Result<()>> + Send + 'static {
    // The tokens around the call reach every read it makes, as one read's do.
    let tokens = cancel::captured();
    let what = format!("index {}: a wait for it to be ready", index.name());
    async move {
        let maintains_itself = index.maintains_itself();
        loop {
            // asked before the look, so that a settle between the two wakes it
            let settled = (!maintains_itself).then(|| {
                let (answer, settled) = pending(what.clone());
                index.settled_then(move |woken| answer.send(woken));
                settled
            });
            let looking = index.clone();
            let look = tokens.run(None, || run::read(&pool, what.clone(), move || looking.is_ready()));
            if look.await? {
                return Ok(());
            }
            match settled {
                Some(settled) => settled.await?,
                None => {
                    let maintaining = index.clone();
                    let pass = run::blocking(&pool, None, what.clone(), move || maintaining.maintain_once());
                    pass.await?;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
