use std::fmt;
use std::sync::Arc;
use std::time::SystemTime;

use serde::de::DeserializeOwned;

use super::batch::{Batch, Ran, Write};
use super::engine::{Base, Databases};
use super::migrations::Migrations;
use super::rows::{Answer, Held, Rows};
use super::run::{self, Read, Wanted};
use super::statement::Sql;
use super::tx::Tx;
#[cfg(any(feature = "kv", feature = "jobs", feature = "blobs", feature = "search"))]
use crate::engine::SharedFile;
use crate::{Durability, Error, Result, Store, Transaction};

impl Store {
    /// The application's database `name`, `sql/<name>.db` in the store's
    /// directory, made the first time it opens:
    ///
    /// ```no_run
    /// # fn main() -> tinystore::Result<()> {
    /// # let store = tinystore::Store::open("data", Default::default())?;
    /// let db = store.database("app").migrations("migrations").open()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn database(&self, name: &str) -> DatabaseBuilder {
        DatabaseBuilder { store: self.clone(), name: name.to_owned(), migrations: None, durability: None }
    }
}

/// How a database opens; [`Store::database`] makes one.
#[derive(Debug)]
#[must_use = "a database opens with `open`"]
pub struct DatabaseBuilder {
    store: Store,
    name: String,
    migrations: Option<Migrations>,
    durability: Option<Durability>,
}

impl DatabaseBuilder {
    /// The migrations the file must have applied: opening applies those it
    /// has not, all in one transaction, and refuses a file that applied an
    /// edited, renamed or missing one, or more than the program has. Without
    /// them the file opens as it is.
    pub fn migrations(mut self, migrations: impl Into<Migrations>) -> Self {
        self.migrations = Some(migrations.into());
        self
    }

    /// How far this database's commits go before they return, where the
    /// store's own setting is not this file's: what may be lost to a power
    /// loss beside what may not. Its buckets and queues commit with it. A
    /// database open already keeps the durability it was opened with, and
    /// asking for another is `Invalid`.
    pub fn durability(mut self, durability: Durability) -> Self {
        self.durability = Some(durability);
        self
    }

    pub fn open(self) -> Result<Database> {
        let describe = super::engine::described(&self.name);
        let durability = self.durability;
        let opened = check_name(&self.name)
            .and_then(|()| self.migrations.map(Migrations::load).transpose())
            .and_then(|migrations| Databases::of(&self.store)?.open(&self.store, &self.name, migrations, durability));
        opened.map(|base| Database { base }).map_err(|error| error.within(describe))
    }
}

/// An application's database: its reads, its writes, its transactions.
/// Clones share the database, which closes with its store.
#[derive(Clone)]
pub struct Database {
    base: Arc<Base>,
}

/// What a write changed: the rows, and SQLite's rowid of the row it inserted,
/// `0` when it inserted none. The rowid is a table's key only for an `integer
/// primary key`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Done {
    pub changes: u64,
    pub last_insert_rowid: i64,
}

/// The calls that read a statement's rows, the same on a database and in its
/// transaction: each type has `rows`, which reads them, and `base`.
macro_rules! reads {
    () => {
        /// The rows a statement gives, each read as a `T`: a struct by its
        /// columns' names, a tuple by their order, a single value when a row has
        /// one column. A write with `returning` runs on the writer and gives its
        /// rows once its commit is durable.
        pub fn all<T: DeserializeOwned>(&self, statement: impl Into<Sql>) -> Result<Vec<T>> {
            let statement = statement.into();
            self.rows(&statement, Wanted::All)
                .and_then(Rows::decode)
                .map_err(|error| self.base.failed(&statement, error))
        }

        /// The one row a statement gives, or none; a statement that gives two is
        /// `invalid`, and a write that gave them rolls back.
        pub fn one<T: DeserializeOwned>(&self, statement: impl Into<Sql>) -> Result<Option<T>> {
            let statement = statement.into();
            let rows = self.rows(&statement, Wanted::One).and_then(Rows::decode);
            rows.map(|rows: Vec<T>| rows.into_iter().next()).map_err(|error| self.base.failed(&statement, error))
        }

        /// The one value of the one row a statement gives, as `count(*)` does;
        /// `Option` takes a `NULL`, as `max` over no rows gives.
        pub fn scalar<T: DeserializeOwned>(&self, statement: impl Into<Sql>) -> Result<T> {
            let statement = statement.into();
            self.rows(&statement, Wanted::Scalar)
                .and_then(Rows::scalar)
                .map_err(|error| self.base.failed(&statement, error))
        }

        /// A statement's rows as SQLite keeps them, for a caller that is not
        /// Rust: the wire's.
        pub(crate) fn rows_of(&self, statement: &Sql, wanted: Wanted) -> Result<Rows> {
            self.rows(statement, wanted).map_err(|error| self.base.failed(statement, error))
        }
    };
}
pub(super) use reads;

impl Database {
    pub fn name(&self) -> &str {
        &self.base.name
    }

    reads!();

    /// Runs a write once it is durable. Writes from many callers share a
    /// commit, each in a savepoint of it: one that fails rolls back alone.
    pub fn exec(&self, statement: impl Into<Sql>) -> Result<Done> {
        let statement = statement.into();
        #[cfg(feature = "replication")]
        if self.base.file().is_copy() {
            let done = self.pass_on(std::slice::from_ref(&statement), false)?;
            return done.into_iter().next().ok_or_else(|| Error::internal("a write's home answered no result"));
        }
        let weight = statement.weight();
        let running = statement.clone();
        self.base
            .file()
            .write(weight, move |tx| run::exec(tx, &running))
            .map_err(|error| self.base.failed(&statement, error))
    }

    /// Runs writes known before they run as one, all of them or none, in one
    /// savepoint of the next shared commit: it costs what one write does, and
    /// gives what each did, in their order. A write is a statement, or a job's
    /// add of a queue opened from the database, `queue.adding(&job)`, which
    /// commits with the rows and changes one job, or none when its id was
    /// taken; see [`Write`].
    pub fn batch<I>(&self, writes: I) -> Result<Vec<Done>>
    where
        I: IntoIterator,
        I::Item: Into<Write>,
    {
        let owner = self.base.describe();
        let writes = writes.into_iter().map(Into::into).collect();
        let batch = Batch::checked(writes, self.base.file(), &owner).map_err(|error| error.within(&owner))?;
        #[cfg(feature = "replication")]
        if self.base.file().is_copy() {
            let statements = batch.statements();
            let done = self.pass_on(&statements, true)?;
            if done.len() != statements.len() {
                let why = format!("a batch of {} statements answered with {} results", statements.len(), done.len());
                return Err(Error::internal(why));
            }
            return Ok(done);
        }
        let (weight, write) = batch.write();
        self.base.file().write(weight, write).map(Ran::finish).map_err(|error| error.within(owner))
    }

    /// Runs `work` in a transaction that holds the writer, for writes that
    /// depend on what its reads found: `Ok` commits, an error or a panic rolls
    /// back. It runs once. Past five seconds it rolls back and is `limit`.
    ///
    /// A bucket or a queue opened from the database joins it through
    /// `tx.with(&handle)`, so that its keys and jobs commit with the rows.
    pub fn tx<T, E: From<Error>>(&self, work: impl FnOnce(&Tx<'_>) -> Result<T, E>) -> Result<T, E> {
        self.tx_since(self.base.now(), work)
    }

    /// `tx`, its bound counted from `started`: when an awaited transaction
    /// was first polled, so that its wait in its database's line counts, as
    /// a blocking one's wait for the writer does.
    pub(crate) fn tx_since<T, E: From<Error>>(
        &self,
        started: SystemTime,
        work: impl FnOnce(&Tx<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        let bound = || self.base.within_bound(started);
        let owner = self.base.describe();
        Transaction::run(self.base.file(), &owner, Some(&bound), |transaction| work(&Tx::new(transaction, &self.base)))
    }

    /// Passes statements to the home of the copy the database's file is, and
    /// gives what each did once this copy holds their commit.
    #[cfg(feature = "replication")]
    fn pass_on(&self, statements: &[Sql], batch: bool) -> Result<Vec<Done>> {
        let store = self.base.shared.store().map_err(|error| error.within(self.base.describe()))?;
        super::forward::pass_on(&store, &self.base.name, statements, batch)
    }

    /// The file the database lends to the buckets, queues, files and indexes
    /// opened from it.
    #[cfg(any(feature = "kv", feature = "jobs", feature = "blobs", feature = "search"))]
    pub(crate) fn shared(&self) -> &Arc<SharedFile> {
        &self.base.shared
    }

    /// `exec` without waiting: `done` is called on the thread that commits it.
    /// A copy passes it on to its home from a thread of its own, which waits
    /// for the home's answer.
    #[cfg(any(feature = "wire", feature = "async"))]
    pub(crate) fn exec_then(&self, statement: Sql, done: impl FnOnce(Result<Done>) + Send + 'static) {
        #[cfg(feature = "replication")]
        if self.base.file().is_copy() {
            let db = self.clone();
            std::thread::spawn(move || done(db.exec(statement)));
            return;
        }
        let (weight, fail) = (statement.weight(), self.failed_by(&statement));
        self.base.file().submit(weight, move |tx| run::exec(tx, &statement), move |ran| done(ran.map_err(fail)));
    }

    /// `batch` without waiting: `done` is called on the thread that commits it,
    /// or, on a copy, on the thread that waits for its home's answer.
    #[cfg(any(feature = "wire", feature = "async"))]
    pub(crate) fn batch_then(&self, writes: Vec<Write>, done: impl FnOnce(Result<Vec<Done>>) + Send + 'static) {
        #[cfg(feature = "replication")]
        if self.base.file().is_copy() {
            let db = self.clone();
            std::thread::spawn(move || done(db.batch(writes)));
            return;
        }
        let owner = self.base.describe();
        let (weight, write) = match Batch::checked(writes, self.base.file(), &owner) {
            Ok(batch) => batch.write(),
            Err(error) => return done(Err(error.within(owner))),
        };
        let ran = move |ran: Result<Ran>| done(ran.map(Ran::finish).map_err(|error| error.within(owner)));
        self.base.file().submit(weight, write, ran);
    }

    /// `rows_of` without waiting for a commit: a read's rows come before this
    /// returns, and a write's once its commit ends, on the thread that
    /// commits it.
    #[cfg(feature = "wire")]
    pub(crate) fn rows_then<A: Answer + Send + 'static>(
        &self,
        statement: Sql,
        wanted: Wanted,
        done: impl FnOnce(Result<A>) + Send + 'static,
    ) {
        match self.probed(&statement, wanted) {
            Ok(Some(rows)) => done(Ok(rows)),
            Ok(None) => self.written_rows_then(statement, wanted, done),
            Err(error) => done(Err(self.failed_by(&statement)(error))),
        }
    }

    /// The rows of a statement known to write, once its commit ends, on the
    /// thread that commits it: `rows_then` past its read, and an awaited
    /// call's write once its read on the pool found that it writes.
    #[cfg(any(feature = "wire", feature = "async"))]
    pub(crate) fn written_rows_then<A: Answer + Send + 'static>(
        &self,
        statement: Sql,
        wanted: Wanted,
        done: impl FnOnce(Result<A>) + Send + 'static,
    ) {
        let (fail, weight, held) = (self.failed_by(&statement), statement.weight(), Held::of(&self.base.memory));
        let write = move |tx: &crate::sqlite::Tx<'_>| run::write(tx, &statement, wanted, held);
        self.base.file().submit(weight, write, move |rows| done(rows.map_err(fail)));
    }

    /// A statement's rows from a reader alone, for a connection that may not
    /// write: none when SQLite says the statement writes, or when it is not a
    /// query, since SQLite counts an ATTACH, which opens a file and stays on
    /// the shared reader, and a PRAGMA that changes the reader, as reads.
    #[cfg(feature = "wire")]
    pub(crate) fn read<A: Answer>(&self, statement: &Sql, wanted: Wanted) -> Result<Option<A>> {
        if !is_query(statement.text()) {
            return Ok(None);
        }
        self.probed(statement, wanted).map_err(|error| self.base.failed(statement, error))
    }

    /// A statement's rows from a reader, or from the writer when SQLite says
    /// it writes; a statement known to write goes to the writer at once.
    fn rows(&self, statement: &Sql, wanted: Wanted) -> Result<Rows> {
        if let Some(rows) = self.probed(statement, wanted)? {
            return Ok(rows);
        }
        let (running, held) = (statement.clone(), Held::of(&self.base.memory));
        self.base.file().write(statement.weight(), move |tx| run::write(tx, &running, wanted, held))
    }

    /// A statement's rows from a reader, or none when it writes: one known
    /// to write is not read, and one SQLite finds writes is remembered, so
    /// that the next goes to the writer at once.
    pub(crate) fn probed<A: Answer>(&self, statement: &Sql, wanted: Wanted) -> Result<Option<A>> {
        if self.base.writes(statement.text()) {
            return Ok(None);
        }
        let held = Held::of(&self.base.memory);
        match self.base.file().read(|connection| run::read(connection, statement, wanted, held))? {
            Read::Rows(rows) => Ok(Some(rows)),
            Read::Writes => {
                self.base.remember_write(statement.text());
                Ok(None)
            }
        }
    }

    /// Whether the database knows `statement` writes, so that a read of it
    /// goes to the writer without asking a reader first.
    #[cfg(feature = "async")]
    pub(crate) fn known_to_write(&self, statement: &Sql) -> bool {
        self.base.writes(statement.text())
    }

    /// What names a failure of `statement`, to say it on another thread.
    #[cfg(any(feature = "wire", feature = "async"))]
    pub(crate) fn failed_by(&self, statement: &Sql) -> impl Fn(Error) -> Error + Clone + Send + Sync + use<> {
        let (statement, database) = (statement.describe(), self.base.describe());
        move |error| error.within(&statement).within(&database)
    }

    /// The line of the store's pool the database's transactions wait in, one
    /// at a time and in their order, whether a connection or an awaited call
    /// asked for them.
    #[cfg(any(feature = "wire", feature = "async"))]
    pub(crate) fn line(&self) -> String {
        self.base.describe()
    }

    /// The database as an error names it: `sql app`.
    #[cfg(feature = "async")]
    pub(crate) fn describe(&self) -> String {
        self.base.describe()
    }

    #[cfg(feature = "async")]
    pub(crate) fn now(&self) -> SystemTime {
        self.base.now()
    }

    /// The store the database is open in, while it is open.
    #[cfg(feature = "async")]
    pub(crate) fn store(&self) -> Result<Store> {
        self.base.shared.store()
    }
}

impl fmt::Debug for Database {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Database({})", self.base.name)
    }
}

/// A database's name is `[a-z0-9][a-z0-9_-]{0,63}`: lower case, so that it
/// cannot climb out of `sql/` or meet another on a file system that folds case.
fn check_name(name: &str) -> Result<()> {
    crate::text::check_name("a database's", name)
}

/// Whether a statement may be a query: its first word, past spaces and
/// comments, one a query begins with. A `with` begins a write too, which
/// SQLite tells when it compiles the statement.
///
/// ```text
/// "/* hourly */ with t as (select 1) select * from t"  → a query
/// "attach database 'other.db' as other"                 → not one
/// ```
#[cfg(any(feature = "wire", test))]
pub(super) fn is_query(text: &str) -> bool {
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        if let Some(line) = rest.strip_prefix("--") {
            rest = line.split_once('\n').map_or("", |(_, after)| after);
        } else if let Some(block) = rest.strip_prefix("/*") {
            rest = block.split_once("*/").map_or("", |(_, after)| after);
        } else {
            break;
        }
    }
    let word: String = rest.chars().take_while(char::is_ascii_alphabetic).collect();
    ["select", "with", "values", "explain"].iter().any(|query| word.eq_ignore_ascii_case(query))
}
