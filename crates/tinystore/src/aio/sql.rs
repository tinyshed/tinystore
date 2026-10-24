//! The async API's handles of the SQL engine, beside the blocking ones of
//! [`crate::sql`](mod@crate::sql): a database's reads, writes, batches and
//! transactions, and its tables' queries, each awaited.
//!
//! ```no_run
//! # use tinystore::sql;
//! # #[derive(serde::Deserialize)]
//! # struct Note { id: String, title: String, done: bool }
//! # async fn example() -> tinystore::Result<()> {
//! # let user_id = 42;
//! let store = tinystore::aio::Store::open("data", Default::default())?;
//! let db = store.database("app").migrations("migrations").open()?; // plain: a first open migrates
//! db.exec(sql!("insert into notes (id, author_id, title) values (?, ?, ?)", "0192f2a4", user_id, "Buy milk")).await?;
//! let mine: Vec<Note> = db.all(sql!("select id, title, done from notes where author_id = ?", user_id)).await?;
//! # Ok(())
//! # }
//! ```
//!
//! The handles are new types with the blocking names; everything else is
//! [`crate::sql`](mod@crate::sql)'s, used as it is: [`sql!`](crate::sql!)
//! and [`Sql`], the pieces such as [`sql::eq`], [`Done`], [`Page`],
//! [`Upsert`], [`Direction`], a batch's [`Write`] and a transaction's [`Tx`].
//!
//! # Where the work runs
//!
//! | A call                         | Runs                                      | Its future dropped once polled   |
//! |--------------------------------|-------------------------------------------|----------------------------------|
//! | a read, `db.all`, `query.list` | on the store's pool                       | stops, its statement too         |
//! | a read whose statement writes  | read on the pool, which queues its write  | commits once its read found that |
//! | a write, `db.exec`, `insert`   | queued in its first poll, led on the pool | commits                          |
//! | a transaction, `db.tx`         | its closure, on the pool, in a line       | runs to its end                  |
//!
//! A read is a database's `all`, `one` and `scalar`, and a query's `all`,
//! `one`, `scalar`, `count`, `explain` and `list`: dropped, its statement is
//! interrupted and its reader given back. A statement that writes, as one
//! with `returning` does, is queued at the writer by the read that finds it
//! writes, and from then on it commits as a write does; one the database
//! knows writes is queued in its first poll. A write is a database's `exec`
//! and `batch`, and a table's `insert`, `insert_all`, `upsert`, `update` and
//! `delete`: polled, it commits with its group whether or not its future is
//! still awaited.
//!
//! None commits, syncs the disk or waits on the thread that polls its
//! future: a commit's lead goes to the store's pool. A call converts its
//! arguments and clones its handle when it is made, so that its future
//! borrows nothing and a program can spawn it; it does nothing before its
//! first poll. Rows are decoded as `T` on the store's pool, a read's on the
//! thread that read them and a write's once its commit ended, so a row's
//! type is `Send + 'static`.

use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::oneshot::{Answer, Pending, pending};
use super::run;
use crate::polled;
use crate::pool::Pool;
use crate::sql::{self, Direction, Done, Migrations, Page, Paging, Parts, Rows, Sql, Tx, Upsert, Wanted, Write};
use crate::{Durability, Error, Result};

impl super::Store {
    /// The application's database `name`, `sql/<name>.db` in the store's
    /// directory, as [`crate::Store::database`] opens it:
    ///
    /// ```no_run
    /// # async fn example() -> tinystore::Result<()> {
    /// # let store = tinystore::aio::Store::open("data", Default::default())?;
    /// let db = store.database("app").migrations("migrations").open()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn database(&self, name: &str) -> DatabaseBuilder {
        DatabaseBuilder { builder: self.blocking().database(name), pool: Arc::clone(self.pool()) }
    }
}

/// How a database opens; [`Store::database`](super::Store::database) makes
/// one, with the blocking builder's options.
#[derive(Debug)]
#[must_use = "a database opens with `open`"]
pub struct DatabaseBuilder {
    builder: sql::DatabaseBuilder,
    pool: Arc<Pool>,
}

impl DatabaseBuilder {
    /// The migrations the file must have applied, as
    /// [`sql::DatabaseBuilder::migrations`] takes them.
    pub fn migrations(self, migrations: impl Into<Migrations>) -> Self {
        DatabaseBuilder { builder: self.builder.migrations(migrations), pool: self.pool }
    }

    /// How far its commits go before they return, as
    /// [`sql::DatabaseBuilder::durability`] says.
    pub fn durability(self, durability: Durability) -> Self {
        DatabaseBuilder { builder: self.builder.durability(durability), pool: self.pool }
    }

    /// Opens the database as [`sql::DatabaseBuilder::open`] does: a plain
    /// call, made once at start, since a first open applies and checks the
    /// migrations.
    pub fn open(self) -> Result<Database> {
        self.builder.open().map(|db| Database { db, pool: self.pool })
    }
}

/// An application's database whose calls are awaited: the same file,
/// writer and readers as [`sql::Database`], and the same database when one
/// is made from the other. Clones share it, and it closes with its store.
#[derive(Clone)]
pub struct Database {
    db: sql::Database,
    pool: Arc<Pool>,
}

impl Database {
    pub fn name(&self) -> &str {
        self.db.name()
    }

    /// The same database, for its blocking calls, and to open a bucket, a
    /// queue or a table's index kept in its file.
    pub fn blocking(&self) -> &sql::Database {
        &self.db
    }

    /// The rows a statement gives, each read as a `T`, as
    /// [`sql::Database::all`] reads them. A statement that writes, as one
    /// with `returning` does, is queued at the writer by the read that finds
    /// it writes, and from then on it commits whether or not its future is
    /// still awaited.
    pub fn all<T>(&self, statement: impl Into<Sql>) -> impl Future<Output = Result<Vec<T>>> + Send + 'static
    where
        T: DeserializeOwned + Send + 'static,
    {
        self.rows(statement.into(), Wanted::All, Rows::decode::<T>)
    }

    /// The one row a statement gives, or none, as [`sql::Database::one`]
    /// reads it; it runs as [`Database::all`] does.
    pub fn one<T>(&self, statement: impl Into<Sql>) -> impl Future<Output = Result<Option<T>>> + Send + 'static
    where
        T: DeserializeOwned + Send + 'static,
    {
        self.rows(statement.into(), Wanted::One, |rows| Ok(rows.decode::<T>()?.into_iter().next()))
    }

    /// The one value of the one row a statement gives, as
    /// [`sql::Database::scalar`] reads it; it runs as [`Database::all`] does.
    pub fn scalar<T>(&self, statement: impl Into<Sql>) -> impl Future<Output = Result<T>> + Send + 'static
    where
        T: DeserializeOwned + Send + 'static,
    {
        self.rows(statement.into(), Wanted::Scalar, Rows::scalar::<T>)
    }

    /// Runs a write once it is durable, sharing a commit with the writes of
    /// other callers, as [`sql::Database::exec`] does. On a copy it is passed
    /// to the home from a thread of its own, which waits for the home's
    /// answer.
    pub fn exec(&self, statement: impl Into<Sql>) -> impl Future<Output = Result<Done>> + Send + 'static {
        self.exec_of(Ok(statement.into()))
    }

    /// Runs writes known before they run as one, all of them or none, as
    /// [`sql::Database::batch`] does: statements, and jobs' adds of queues
    /// opened from the database, `queue.adding(&job)`.
    pub fn batch<I>(&self, writes: I) -> impl Future<Output = Result<Vec<Done>>> + Send + 'static
    where
        I: IntoIterator,
        I::Item: Into<Write>,
    {
        let (db, writes) = (self.db.clone(), writes.into_iter().map(Into::into).collect::<Vec<Write>>());
        run::write(&self.pool, self.what("a batch"), move |answer| {
            db.batch_then(writes, move |done| answer.send(done));
        })
    }

    /// Runs `work` in a transaction that holds the writer, as
    /// [`sql::Database::tx`] does, on a thread of the store's pool: `work` is
    /// blocking, so it must not wait for the program's awaits, and it runs on
    /// no executor's thread. Inside, a bucket or a queue opened from the
    /// database joins it through `tx.with(&handle)`.
    ///
    /// The database's transactions wait in one line, one at a time in the
    /// order they were polled, whether awaited or asked for over the wire;
    /// those waiting hold no thread. Its five seconds count from its first
    /// poll, its wait in the line among them. A panic in `work` rolls the
    /// transaction back and is resumed at the `.await`.
    ///
    /// ```no_run
    /// # use tinystore::sql;
    /// # async fn example(db: tinystore::aio::sql::Database, flight_id: i64) -> tinystore::Result<()> {
    /// let booked = db
    ///     .tx(move |tx| -> tinystore::Result<bool> {
    ///         let left: i64 = tx.scalar(sql!("select seats_left from flights where id = ?", flight_id))?;
    ///         if left == 0 {
    ///             return Ok(false); // sold out: nothing is written
    ///         }
    ///         tx.exec(sql!("update flights set seats_left = seats_left - 1 where id = ?", flight_id))?;
    ///         Ok(true)
    ///     })
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn tx<T, E, F>(&self, work: F) -> impl Future<Output = Result<T, E>> + Send + 'static
    where
        F: FnOnce(&Tx<'_>) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<Error> + Send + 'static,
    {
        let database = self.clone();
        async move {
            let started = database.db.now();
            let ran = database.run_in_line("a transaction", move |db| Ok(db.tx_since(started, work)));
            ran.await.unwrap_or_else(|error| Err(E::from(error)))
        }
    }

    /// Runs `work` on the store's pool in the database's line, after what
    /// was polled before it, whether awaited or asked for over the wire:
    /// what holds the file's writer alone waits there on no thread, a
    /// transaction or a drop of a table's index. Once polled it runs to its
    /// end, whether or not its future is still awaited.
    pub(crate) fn run_in_line<T, F>(
        &self,
        call: &str,
        work: F,
    ) -> impl Future<Output = Result<T>> + Send + 'static + use<T, F>
    where
        F: FnOnce(&sql::Database) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let db = self.db.clone();
        run::blocking(&self.pool, Some(db.line()), self.what(call), move || work(&db))
    }

    /// A table of the database, typed where it opens, as
    /// [`sql::Database::table`] gives one: its inserts and upserts, and its
    /// queries, each awaited. It costs nothing until its first call.
    pub fn table<T>(&self, name: &str) -> Table<T> {
        Table { query: Query { database: self.clone(), parts: Parts::of_table(name), row: PhantomData } }
    }

    /// A statement's answer, which `finish` makes of its rows on the store's
    /// pool. A statement the database knows writes is queued at the writer
    /// in the first poll. Any other is read on the pool first, and the read
    /// that finds it writes queues it there and then: from that moment it
    /// commits whether or not its future is still awaited (DUR-11).
    fn rows<A, F>(&self, statement: Sql, wanted: Wanted, finish: F) -> impl Future<Output = Result<A>> + Send + 'static
    where
        A: Send + 'static,
        F: FnOnce(Rows) -> Result<A> + Send + 'static,
    {
        let start = if self.db.known_to_write(&statement) {
            Start::Written(self.written(statement, wanted, finish))
        } else {
            // made here, where the caller's tokens are, so that they reach the read on the pool
            let database = self.clone();
            let read = move || database.probed((statement, wanted), finish);
            Start::Read(run::read(&self.pool, self.what("a read"), read))
        };
        async move {
            match start {
                Start::Written(written) => written.await,
                Start::Read(read) => match read.await? {
                    Probed::Answered(answer) => Ok(answer),
                    Probed::Queued(written) => written.await,
                },
            }
        }
    }

    /// A statement known to write, queued at the writer in its first poll.
    fn written<A, F>(
        &self,
        statement: Sql,
        wanted: Wanted,
        finish: F,
    ) -> impl Future<Output = Result<A>> + Send + 'static
    where
        A: Send + 'static,
        F: FnOnce(Rows) -> Result<A> + Send + 'static,
    {
        let database = self.clone();
        let queue = move |answer| database.queue_write((statement, wanted), finish, answer);
        run::write(&self.pool, self.what("a write"), queue)
    }

    /// Reads a statement on a reader and makes its answer there; run on the
    /// pool. One that writes is queued at the writer from this thread, its
    /// commit led on the pool, before the read answers: a future dropped once
    /// its read found that it writes still commits it.
    fn probed<A, F>(&self, (statement, wanted): (Sql, Wanted), finish: F) -> Result<Probed<A>>
    where
        A: Send + 'static,
        F: FnOnce(Rows) -> Result<A> + Send + 'static,
    {
        let fail = self.db.failed_by(&statement);
        match self.db.probed::<Rows>(&statement, wanted) {
            Ok(Some(rows)) => finish(rows).map(Probed::Answered).map_err(fail),
            Ok(None) => {
                let (answer, answered) = pending(self.what("a write"));
                // a cancel stops reads alone: the token this read runs under stops nothing of the write
                polled::leading_on(&run::lead_on(&self.pool), || self.queue_write((statement, wanted), finish, answer));
                Ok(Probed::Queued(answered))
            }
            Err(error) => Err(fail(error)),
        }
    }

    /// Queues a statement that writes at the writer, and sends `answer` what
    /// `finish` makes of its rows once its commit ends. Called inside
    /// `polled::leading_on`, so that a commit it would lead goes to the pool.
    ///
    /// `finish` runs on the pool, never on the thread that led the commit:
    /// that thread answers every write of the commit before it leads the
    /// next, and it may be a blocking caller's, so decoding the rows of a
    /// large `insert_all` there would hold back every write queued behind
    /// them. The decoding runs even as the store closes, since the write it
    /// answers committed, and not at all once the future is gone.
    fn queue_write<A, F>(&self, (statement, wanted): (Sql, Wanted), finish: F, answer: Answer<A>)
    where
        A: Send + 'static,
        F: FnOnce(Rows) -> Result<A> + Send + 'static,
    {
        let (fail, pool) = (self.db.failed_by(&statement), Arc::clone(&self.pool));
        self.db.written_rows_then(statement, wanted, move |rows: Result<Rows>| match rows {
            Ok(rows) => pool.run_always(Box::new(move || {
                if !answer.is_awaited() {
                    return;
                }
                match catch_unwind(AssertUnwindSafe(|| finish(rows).map_err(fail))) {
                    Ok(made) => answer.send(made),
                    Err(payload) => answer.panicked(payload),
                }
            })),
            Err(error) => answer.send(Err(error)),
        });
    }

    /// A write that gives no rows, or what refused it before it was made,
    /// answered in its first poll.
    fn exec_of(&self, statement: Result<Sql>) -> impl Future<Output = Result<Done>> + Send + 'static {
        let db = self.db.clone();
        run::write(&self.pool, self.what("a write"), move |answer| match statement {
            Ok(statement) => db.exec_then(statement, move |done| answer.send(done)),
            Err(error) => answer.send(Err(error)),
        })
    }

    /// A call of the database as its errors name it: `sql app: a read`.
    fn what(&self, call: &str) -> String {
        format!("{}: {call}", self.db.describe())
    }
}

/// How a statement's answer starts: a read on the pool, or, for a statement
/// the database knows writes, its write.
enum Start<R, W> {
    Read(R),
    Written(W),
}

/// What a read of a statement found: its answer, or that it writes, and the
/// answer of the write it queued.
enum Probed<A> {
    Answered(A),
    Queued(Pending<A>),
}

impl From<sql::Database> for Database {
    /// The same database, its calls awaited: a clone of its handle, never a
    /// second database. One whose store closed answers every call `Closed`.
    fn from(db: sql::Database) -> Database {
        let pool = db.store().map_or_else(|_| Pool::stopped(), |store| Pool::of_or_stopped(&store));
        Database { db, pool }
    }
}

impl fmt::Debug for Database {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.db, f)
    }
}

/// A query on a table whose calls are awaited, built as
/// [`sql::Query`] is: each step returns a new query, so that the first
/// stays as it was. `all`, `one`, `scalar`, `count`, `explain` and `list`
/// read on the store's pool; `update` and `delete` write.
///
/// ```no_run
/// # use tinystore::sql::{self, Direction};
/// # #[derive(serde::Serialize, serde::Deserialize)] struct Order { id: String }
/// # async fn example(db: tinystore::aio::sql::Database) -> tinystore::Result<()> {
/// let urgent: Vec<Order> = db
///     .table::<Order>("orders")
///     .filter(sql::eq("status", "pending"))
///     .order_by("created_at", Direction::Desc)
///     .limit(50)
///     .all()
///     .await?;
/// # Ok(())
/// # }
/// ```
pub struct Query<T> {
    database: Database,
    parts: Parts,
    row: PhantomData<fn() -> T>,
}

/// A table whose calls are awaited: its queries, and its rows' inserts and
/// upserts, as [`sql::Table`] has them. `Deref`s to the query of all its
/// rows.
pub struct Table<T> {
    query: Query<T>,
}

impl<T> std::ops::Deref for Table<T> {
    type Target = Query<T>;

    fn deref(&self) -> &Query<T> {
        &self.query
    }
}

impl<T> Table<T> {
    pub fn name(&self) -> &str {
        self.query.parts.table()
    }
}

impl<T: Serialize + DeserializeOwned + Send + 'static> Table<T> {
    /// Inserts a row and gives it back as the file keeps it, as
    /// [`sql::Table::insert`] does. The row is encoded when the call is made.
    pub fn insert(&self, row: &T) -> impl Future<Output = Result<T>> + Send + 'static {
        let inserted = self.insert_all(std::slice::from_ref(row));
        async move { inserted.await?.pop().ok_or_else(|| Error::internal("an insert that gave back no row")) }
    }

    /// Inserts rows of one shape as one write, all of them or none, as
    /// [`sql::Table::insert_all`] does.
    pub fn insert_all(&self, rows: &[T]) -> impl Future<Output = Result<Vec<T>>> + Send + 'static {
        let statement = (!rows.is_empty()).then(|| sql::insert_sql(self.name(), rows)).transpose();
        let written = statement.map(|statement| {
            statement.map(|statement| self.query.database.written(statement, Wanted::All, Rows::decode::<T>))
        });
        async move {
            match written? {
                Some(written) => written.await,
                None => Ok(Vec::new()),
            }
        }
    }

    /// Inserts a row, or updates the row that holds its key, as
    /// [`sql::Table::upsert`] does; `None` when the upsert's condition kept
    /// that row.
    pub fn upsert(&self, row: &T, upsert: Upsert) -> impl Future<Output = Result<Option<T>>> + Send + 'static {
        let written = sql::upsert_sql(self.name(), row, upsert).map(|statement| {
            self.query.database.written(statement, Wanted::One, |rows| Ok(rows.decode::<T>()?.into_iter().next()))
        });
        async move { written?.await }
    }
}

impl<T> Query<T> {
    /// Adds a condition with `and`, as [`sql::Query::filter`] does.
    #[must_use]
    pub fn filter(&self, condition: impl Into<Sql>) -> Self {
        self.with(|parts| parts.filter(condition.into()))
    }

    /// The columns a row holds, as SQL text, and the shape of the row they
    /// make, as [`sql::Query::select`] says.
    pub fn select<S>(&self, columns: impl Into<Sql>) -> Query<S> {
        let mut parts = self.parts.clone();
        parts.select(columns.into());
        Query { database: self.database.clone(), parts, row: PhantomData }
    }

    /// Gives each row the rows of another query as a list under `name`, as
    /// [`sql::Query::include`] does, in the same statement.
    #[must_use]
    pub fn include<C>(&self, name: &str, query: &Query<C>) -> Self {
        self.with(|parts| parts.include(name, &query.parts))
    }

    /// Joins a table, `"users as u"`, on a condition.
    #[must_use]
    pub fn join(&self, table: &str, on: impl Into<Sql>) -> Self {
        self.with(|parts| parts.join(table, on.into()))
    }

    /// Joins a table whose columns may be null where no row of it meets the condition.
    #[must_use]
    pub fn left_join(&self, table: &str, on: impl Into<Sql>) -> Self {
        self.with(|parts| parts.left_join(table, on.into()))
    }

    /// Groups the rows by columns, quoted as names.
    #[must_use]
    pub fn group_by<I: IntoIterator<Item = S>, S: AsRef<str>>(&self, columns: I) -> Self {
        self.with(|parts| parts.group_by(columns))
    }

    /// A condition on the groups, as `filter` takes one.
    #[must_use]
    pub fn having(&self, condition: impl Into<Sql>) -> Self {
        self.with(|parts| parts.having(condition.into()))
    }

    /// Orders the rows by a column, quoted as a name.
    #[must_use]
    pub fn order_by(&self, column: &str, direction: Direction) -> Self {
        self.with(|parts| parts.order_by(column, direction))
    }

    /// Orders the rows by a piece of SQL: `sql!("lower(title)")`.
    #[must_use]
    pub fn order_by_sql(&self, piece: impl Into<Sql>, direction: Direction) -> Self {
        self.with(|parts| parts.order_by_sql(piece.into(), direction))
    }

    #[must_use]
    pub fn limit(&self, rows: u64) -> Self {
        self.with(|parts| parts.limit(rows))
    }

    #[must_use]
    pub fn offset(&self, rows: u64) -> Self {
        self.with(|parts| parts.offset(rows))
    }

    /// The text and the values the query sends, to read or to log.
    pub fn to_sql(&self) -> Result<Sql> {
        self.parts.to_sql()
    }

    /// How many rows the conditions match, or how many groups, as
    /// [`sql::Query::count`] counts them.
    pub fn count(&self) -> impl Future<Output = Result<u64>> + Send + 'static {
        let counted =
            self.parts.count_sql().map(|statement| self.database.rows(statement, Wanted::Scalar, Rows::scalar));
        async move { counted?.await }
    }

    /// SQLite's plan for the query, a line a step.
    pub fn explain(&self) -> impl Future<Output = Result<Vec<String>>> + Send + 'static {
        let planned =
            self.parts.explain_sql().map(|statement| self.database.rows(statement, Wanted::All, sql::plan_of));
        async move { planned?.await }
    }

    /// Sets columns of the rows the conditions match, as
    /// [`sql::Query::update`] does, and says how many changed.
    pub fn update<I, S>(&self, sets: I) -> impl Future<Output = Result<Done>> + Send + 'static
    where
        I: IntoIterator<Item = (S, Sql)>,
        S: AsRef<str>,
    {
        self.database.exec_of(self.parts.update_sql(sets))
    }

    /// Deletes the rows the conditions match, and says how many it deleted.
    pub fn delete(&self) -> impl Future<Output = Result<Done>> + Send + 'static {
        self.database.exec_of(self.parts.delete_sql())
    }

    fn with(&self, change: impl FnOnce(&mut Parts)) -> Self {
        let mut parts = self.parts.clone();
        change(&mut parts);
        Query { database: self.database.clone(), parts, row: PhantomData }
    }
}

impl<T: DeserializeOwned + Send + 'static> Query<T> {
    pub fn all(&self) -> impl Future<Output = Result<Vec<T>>> + Send + 'static {
        let nested = self.parts.nested();
        self.read(Wanted::All, move |rows| rows.nesting(nested).decode())
    }

    pub fn one(&self) -> impl Future<Output = Result<Option<T>>> + Send + 'static {
        let nested = self.parts.nested();
        self.read(Wanted::One, move |rows| Ok(rows.nesting(nested).decode::<T>()?.into_iter().next()))
    }

    pub fn scalar<V: DeserializeOwned + Send + 'static>(&self) -> impl Future<Output = Result<V>> + Send + 'static {
        self.read(Wanted::Scalar, Rows::scalar::<V>)
    }

    /// A page after the last row of the one before, by the query's order, as
    /// [`sql::Query::list`] reads one; its `next` reads the next page, and
    /// only of this query.
    pub fn list(&self, limit: u64, after: Option<&str>) -> impl Future<Output = Result<Page<T>>> + Send + 'static {
        let page = self.parts.page_sql(limit, after).map(|(statement, paging): (Sql, Paging)| {
            self.database.rows(statement, Wanted::All, move |rows| paging.page(rows))
        });
        async move { page?.await }
    }

    /// The query's statement read, `finish` making the answer of its rows.
    fn read<A, F>(&self, wanted: Wanted, finish: F) -> impl Future<Output = Result<A>> + Send + 'static
    where
        A: Send + 'static,
        F: FnOnce(Rows) -> Result<A> + Send + 'static,
    {
        let read = self.parts.to_sql().map(|statement| self.database.rows(statement, wanted, finish));
        async move { read?.await }
    }
}

impl<T> Clone for Query<T> {
    fn clone(&self) -> Self {
        Query { database: self.database.clone(), parts: self.parts.clone(), row: PhantomData }
    }
}

impl<T> Clone for Table<T> {
    fn clone(&self) -> Self {
        Table { query: self.query.clone() }
    }
}

impl<T> fmt::Debug for Query<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Query").field("database", &self.database).field("parts", &self.parts).finish()
    }
}

impl<T> fmt::Debug for Table<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Table").field("query", &self.query).finish()
    }
}

#[cfg(test)]
#[path = "sql_tests.rs"]
mod tests;
