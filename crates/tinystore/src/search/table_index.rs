//! A table's index: a database's table whose rows are found again by their
//! words, their vectors or both, kept in the database's file and changed in
//! the commit that changes a row.

use std::sync::{Arc, Weak};
use std::time::Duration;

use rusqlite::Connection;
use rusqlite::types::Value as SqlValue;
use serde::de::DeserializeOwned;

use super::engine::{FILE, InFiles, Search};
use super::index::{Found, Hit, Index, Rebuild, SearchCall};
use super::open::{OpenIndex, Status};
use super::read::{Answer, Query};
use super::settings::{Settings, Table, Wanted};
use super::table;
use crate::engine::{Host, SharedFile};
use crate::sqlite::{Settle, sql_error};
use crate::transaction::sealed::Sealed;
use crate::{Error, Result, Transaction, TxHandle};

/// How often a database's file is looked at for keys a process beside the
/// store left, when no write of the store's settled them sooner.
const PENDING: Duration = Duration::from_secs(1);

const READ_COLUMNS: &str = "select name, pk from pragma_table_info(?1)";
const UNIQUE_BY: &str = "select exists (select 1 from pragma_index_list(?1) as list
    where list.\"unique\" and not list.partial
    and (select count(*) from pragma_index_info(list.name)) = 1
    and (select name from pragma_index_info(list.name)) = ?2)";

impl Search {
    /// The search engine kept in a database's file, which its tables'
    /// indexes open in: every transaction of the file settles their pending
    /// keys before it commits, and a look each second those a process
    /// beside the store left.
    pub(crate) fn in_file(shared: &SharedFile) -> Result<Arc<Search>> {
        shared.engine(|store| {
            let owner = shared.owner().to_owned();
            let search = Search::start(store, Arc::clone(shared.file()), Some(owner.clone()), None)?;
            shared.file().settle_with(Arc::downgrade(&search) as Weak<dyn Settle>);
            InFiles::of(store)?.keep(&search);
            let weak = Arc::downgrade(&search);
            store.every(&format!("search in {owner}: pending keys"), PENDING, move || {
                weak.upgrade().map_or(Ok(()), |search| search.settle_beside())
            })?;
            Ok(search)
        })
    }

    /// Settles the keys a process beside the store left, when no write of
    /// the store's has since: a transaction settles them before it commits.
    fn settle_beside(&self) -> Result<()> {
        if self.file.read(table::any_pending)? {
            self.file.transaction(|_| Ok::<(), Error>(()))?;
        }
        Ok(())
    }

    /// The file as an error names it: `search.db`, or `sql app`.
    pub(crate) fn place(&self) -> &str {
        self.lent_by.as_deref().unwrap_or(FILE)
    }
}

/// The table `wanted` names as the database holds it: its key the column
/// asked for or the primary key, every column read there, and the key's
/// values unique.
pub(crate) fn resolve(connection: &Connection, table: &Table, read: &[&str]) -> Result<Table> {
    let failed = |error| sql_error(format!("read table {}", table.name), error);
    let mut select = connection.prepare_cached(READ_COLUMNS).map_err(failed)?;
    let rows = select.query_map([&table.name], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)));
    let columns: Vec<(String, i64)> = rows.and_then(Iterator::collect).map_err(failed)?;
    if columns.is_empty() {
        return Err(Error::invalid(format!("the database has no table {}", table.name)));
    }
    let primary: Vec<&String> = columns.iter().filter(|(_, pk)| *pk > 0).map(|(name, _)| name).collect();
    let key = match (table.key.as_str(), primary.as_slice()) {
        ("", [only]) => (*only).clone(),
        ("", _) => {
            return Err(Error::invalid(format!(
                "table {} has no primary key of one column: name a column of unique values with key(…)",
                table.name
            )));
        }
        (key, _) => key.to_owned(),
    };
    let has = |column: &str| columns.iter().any(|(name, _)| name == column);
    let vector = table.vector.as_deref();
    if let Some(missing) = std::iter::once(key.as_str()).chain(read.iter().copied()).chain(vector).find(|c| !has(c)) {
        return Err(Error::invalid(format!("table {} has no column {missing}", table.name)));
    }
    let unique = primary.as_slice() == [&key] || {
        let mut select = connection.prepare_cached(UNIQUE_BY).map_err(failed)?;
        select.query_row((&table.name, &key), |row| row.get::<_, bool>(0)).map_err(failed)?
    };
    if !unique {
        return Err(Error::invalid(format!(
            "column {key} of table {} is no key: its values are not unique, by a primary key or a unique index",
            table.name
        )));
    }
    Ok(Table { name: table.name.clone(), key, vector: table.vector.clone() })
}

/// How a hit's row is read as a `T`, in the snapshot its search read: the
/// statement that selects it whole, and its key. The database lends it,
/// since a row is read as its own queries read one.
pub(crate) type ReadRow<T> = Arc<dyn Fn(&Connection, &str, &SqlValue) -> Result<Option<T>> + Send + Sync>;

/// What `db.search_index` opens: an index's table and its settings.
#[must_use = "an index opens at `open`"]
pub struct TableIndexBuilder<T> {
    shared: Arc<SharedFile>,
    name: String,
    wanted: Wanted,
    read_row: ReadRow<T>,
}

impl<T> std::fmt::Debug for TableIndexBuilder<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TableIndexBuilder").field("name", &self.name).field("wanted", &self.wanted).finish()
    }
}

impl<T: DeserializeOwned> TableIndexBuilder<T> {
    pub(crate) fn in_file(shared: Arc<SharedFile>, name: &str, read_row: ReadRow<T>) -> Self {
        let table = Table { name: String::new(), key: String::new(), vector: None };
        let wanted = Wanted { table: Some(table), ..Wanted::default() };
        TableIndexBuilder { shared, name: name.to_owned(), wanted, read_row }
    }

    /// The table whose rows the index holds and follows from then on.
    pub fn table(mut self, table: &str) -> Self {
        self.followed().name = table.to_owned();
        self
    }

    /// The column of unique values a row is found by, the table's primary
    /// key unless said; a hit's id is its value as text.
    pub fn key(mut self, column: &str) -> Self {
        self.followed().key = column.to_owned();
        self
    }

    /// A column whose text is read, and what a word found there weighs: a
    /// word in a column of 2 scores twice what it would in a column of 1.
    pub fn field(mut self, column: &str, weight: f64) -> Self {
        self.wanted.fields.push((column.to_owned(), weight));
        self
    }

    /// Columns that each weigh 1.
    pub fn fields<S: AsRef<str>>(mut self, columns: impl IntoIterator<Item = S>) -> Self {
        self.wanted.fields.extend(columns.into_iter().map(|column| (column.as_ref().to_owned(), 1.0)));
        self
    }

    /// The columns a search may keep its hits to, each holding text, a
    /// number, or a boolean as SQLite keeps one, 1 or 0.
    pub fn filters<S: AsRef<str>>(mut self, columns: impl IntoIterator<Item = S>) -> Self {
        self.wanted.filters.extend(columns.into_iter().map(|column| column.as_ref().to_owned()));
        self
    }

    /// Whether a misspelt word is read as the index's nearest: on unless
    /// turned off.
    pub fn typo_tolerance(mut self, on: bool) -> Self {
        self.wanted.typo_tolerance = on;
        self
    }

    /// Reads each row's vector from `column`: a blob of `dimensions`
    /// little-endian floats that the program's own model gave it, or null
    /// for a row without one. The store runs no model: a changed text keeps
    /// its row's vector until the program writes another.
    pub fn vectors(mut self, column: &str, dimensions: usize) -> Self {
        self.followed().vector = Some(column.to_owned());
        self.wanted.dimensions = Some(dimensions);
        self
    }

    /// As [`IndexBuilder::recall`](super::IndexBuilder::recall).
    pub fn recall(mut self, recall: f64) -> Self {
        self.wanted.recall = Some(recall);
        self
    }

    /// As [`IndexBuilder::vector_weight`](super::IndexBuilder::vector_weight).
    pub fn vector_weight(mut self, weight: f64) -> Self {
        self.wanted.vector_weight = Some(weight);
        self
    }

    /// As [`IndexBuilder::rerank`](super::IndexBuilder::rerank).
    pub fn rerank(mut self) -> Self {
        self.wanted.rerank = true;
        self
    }

    /// As [`IndexBuilder::rebuild`](super::IndexBuilder::rebuild).
    pub fn rebuild(mut self, rebuild: Rebuild) -> Self {
        self.wanted.manual = rebuild == Rebuild::Manual;
        self
    }

    /// Opens the index, made the first time: its triggers put on the table,
    /// and its rows read in the background, which [`TableIndex::ready`]
    /// waits for. Other settings change it as [`IndexBuilder::open`](super::IndexBuilder::open)
    /// says; another table or key is refused.
    pub fn open(self) -> Result<TableIndex<T>> {
        self.opened(false)
    }

    /// Opens the index as [`open`](Self::open) does, and reads it again
    /// beside itself in the background.
    pub fn reindex(self) -> Result<TableIndex<T>> {
        self.opened(true)
    }

    fn followed(&mut self) -> &mut Table {
        self.wanted.table.get_or_insert_with(|| Table { name: String::new(), key: String::new(), vector: None })
    }

    fn opened(mut self, reindex: bool) -> Result<TableIndex<T>> {
        let within = |error: Error| error.within(format!("index {}", self.name));
        let asked = self.wanted.table.clone().filter(|table| !table.name.is_empty());
        let asked = asked.ok_or_else(|| within(Error::invalid("a table's index names its table: table(name)")))?;
        self.wanted.check().map_err(within)?;
        let search = Search::in_file(&self.shared).map_err(within)?;
        let read: Vec<&str> = self.wanted.fields.iter().map(|(column, _)| column.as_str()).collect();
        let read: Vec<&str> = read.into_iter().chain(self.wanted.filters.iter().map(String::as_str)).collect();
        let table = search.file.read(|connection| resolve(connection, &asked, &read)).map_err(within)?;
        self.wanted.table = Some(table);
        let open = crate::sqlite::own_work(|| search.open_index(&self.name, &self.wanted, reindex)).map_err(within)?;
        Ok(TableIndex { index: Index::of_table(search, open, self.read_row) })
    }
}

/// The rows of a table found again by their words, their vectors or both;
/// a hit's value is its row, read in the snapshot its search read.
pub struct TableIndex<T> {
    index: Index<T>,
}

impl<T> Clone for TableIndex<T> {
    fn clone(&self) -> Self {
        TableIndex { index: self.index.clone() }
    }
}

impl<T> std::fmt::Debug for TableIndex<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TableIndex").field("name", &self.index.open.name).finish()
    }
}

impl<T: DeserializeOwned> TableIndex<T> {
    pub fn name(&self) -> &str {
        self.index.name()
    }

    /// A search by `text`'s words, as [`Index::search`] makes one; each
    /// hit's id is its row's key and its value the row.
    pub fn search(&self, text: &str) -> SearchCall<'_, T> {
        self.index.search(text)
    }

    /// A search by `vector` alone, as [`Index::search_vector`] makes one.
    pub fn search_vector<'i>(&'i self, vector: &'i [f32]) -> SearchCall<'i, T> {
        self.index.search_vector(vector)
    }

    /// Where the index stands: ready, reading the table's rows the first
    /// time or again beside itself and how far, or a rebuild that failed.
    pub fn status(&self) -> Result<Status> {
        self.index.status()
    }

    /// Waits until the index has read its table's rows and serves the
    /// settings it was opened with.
    pub fn ready(&self) -> Result<()> {
        self.index.ready()
    }

    #[cfg(test)]
    pub(crate) fn inner(&self) -> &Index<T> {
        &self.index
    }

    #[cfg(feature = "wire")]
    pub(crate) fn into_index(self) -> Index<T> {
        self.index
    }
}

impl<T> Sealed for TableIndex<T> {}

impl<T: DeserializeOwned> TxHandle for TableIndex<T> {
    type InTx<'h>
        = TxTableIndex<'h, T>
    where
        Self: 'h;

    fn in_tx<'h>(&'h self, tx: &'h Transaction<'h>) -> TxTableIndex<'h, T> {
        TxTableIndex { index: self, tx }
    }
}

/// A table's index's searches inside a transaction of its database, from
/// `tx.with(&index)`: they see what the transaction wrote.
pub struct TxTableIndex<'a, T> {
    index: &'a TableIndex<T>,
    tx: &'a Transaction<'a>,
}

impl<T> std::fmt::Debug for TxTableIndex<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxTableIndex").field("name", &self.index.index.open.name).finish()
    }
}

impl<'a, T: DeserializeOwned> TxTableIndex<'a, T> {
    pub fn search(&self, text: &str) -> SearchCall<'a, T> {
        self.index.index.search(text).in_tx(self.tx)
    }

    pub fn search_vector(&self, vector: &'a [f32]) -> SearchCall<'a, T> {
        self.index.index.search_vector(vector).in_tx(self.tx)
    }
}

impl<T> Index<T> {
    /// A table's index, whose hits' values are their rows.
    pub(crate) fn of_table(search: Arc<Search>, open: Arc<OpenIndex>, read_row: ReadRow<T>) -> Self {
        Index { search, open, read_row: Some(read_row), _value: std::marker::PhantomData }
    }
}

/// How a row of a table is read, as a `T` or the wire's cells.
pub(crate) type RowReader<'r, R> = &'r dyn Fn(&Connection, &str, &SqlValue) -> Result<Option<R>>;

/// The hits of `query` in a table's index, each with its row as its value,
/// read in the snapshot the search read, or in the transaction `tx` of the
/// table's database, seeing what it wrote. A row a process beside the store
/// deleted that its index has not settled yet is left out.
pub(crate) fn found<T, R>(
    index: &Index<T>,
    read_row: RowReader<'_, R>,
    query: Query,
    tx: Option<&Transaction<'_>>,
) -> Result<Found<R>> {
    let read = |connection: &Connection, settings: &Settings, answer: &Answer| -> Result<Vec<Option<R>>> {
        let table = table_of(settings)?;
        let rows = answer.hits.iter().map(|(_, value, _)| {
            let (select, key) = table::hit_row(&table, value)?;
            read_row(connection, &select, &key)
        });
        rows.collect()
    };
    let (answer, rows) = match tx {
        None => index.search.search_then(&index.open, query, read)?,
        Some(tx) => {
            tx.takes(&index.search.file, index.search.place())?;
            tx.call(|writer| index.search.search_in(writer, &index.open, query, read))?
        }
    };
    let hits = answer.hits.into_iter().zip(rows);
    let hits = hits.filter_map(|((id, _, score), row)| row.map(|value| Hit { id, value, score }));
    Ok(Found { hits: hits.collect(), corrected: answer.corrected })
}

/// Drops the table's index `name` kept in a database's file.
pub(crate) fn drop_in_file(shared: &SharedFile, name: &str) -> Result<bool> {
    let within = |error: Error| error.within(format!("index {name}"));
    Search::in_file(shared).and_then(|search| search.drop_index(name)).map_err(within)
}

/// A search of a table's index whose hits' rows `read_row` reads: the wire
/// sends them by their cells.
#[cfg(feature = "wire")]
pub(crate) fn found_by<T: DeserializeOwned, R>(
    call: SearchCall<'_, T>,
    read_row: RowReader<'_, R>,
) -> Result<Found<R>> {
    let (index, query) = call.asked()?;
    found(index, read_row, query, None)
}

/// The table's index `name` as it is, for a connection that may only read.
#[cfg(feature = "wire")]
pub(crate) fn table_as_it_is<T: DeserializeOwned>(shared: &SharedFile, name: &str) -> Result<Index<T>> {
    let within = |error: Error| error.within(format!("index {name}"));
    let search = Search::in_file(shared).map_err(within)?;
    let open = search.open_existing(name).map_err(within)?;
    if open.current().serving.settings.table.is_none() {
        return Err(within(Error::invalid("an index of documents, which a database keeps none of")));
    }
    Ok(Index::of_table(search, open, by_cells()))
}

/// What a table's index the wire opened reads its rows with: nothing, since
/// the wire reads them by their cells, through [`found_by`].
#[cfg(feature = "wire")]
pub(crate) fn by_cells<T>() -> ReadRow<T> {
    Arc::new(|_, _, _| Err(Error::internal("the wire reads a table's rows by their cells")))
}

/// A table of its settings, for a read of its hits.
fn table_of(settings: &Settings) -> Result<Table> {
    settings.table.clone().ok_or_else(|| Error::internal("a table's index without its table"))
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod tests;
