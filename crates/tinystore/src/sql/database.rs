use std::fmt;
use std::sync::Arc;

use serde::de::DeserializeOwned;

use super::engine::{Base, Databases};
use super::migrations::Migrations;
#[cfg(feature = "wire")]
use super::rows::Answer;
use super::rows::{Held, Rows};
use super::run::{self, Read, Wanted};
use super::statement::Sql;
use super::tx::Tx;
#[cfg(any(feature = "kv", feature = "jobs", feature = "search"))]
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

    /// Runs statements known before they run as one, all of them or none, in
    /// one savepoint of the next shared commit: it costs what one write does.
    pub fn batch<I>(&self, statements: I) -> Result<Vec<Done>>
    where
        I: IntoIterator,
        I::Item: Into<Sql>,
    {
        let statements: Vec<Sql> = statements.into_iter().map(Into::into).collect();
        #[cfg(feature = "replication")]
        if self.base.file().is_copy() {
            let done = self.pass_on(&statements, true)?;
            if done.len() != statements.len() {
                let why = format!("a batch of {} statements answered with {} results", statements.len(), done.len());
                return Err(Error::internal(why));
            }
            return Ok(done);
        }
        let (weight, write) = batch_write(statements);
        self.base.file().write(weight, write).map_err(|error| error.within(self.base.describe()))
    }

    /// Runs `work` in a transaction that holds the writer, for writes that
    /// depend on what its reads found: `Ok` commits, an error or a panic rolls
    /// back. It runs once. Past five seconds it rolls back and is `limit`.
    ///
    /// A bucket or a queue opened from the database joins it through
    /// `tx.with(&handle)`, so that its keys and jobs commit with the rows.
    pub fn tx<T, E: From<Error>>(&self, work: impl FnOnce(&Tx<'_>) -> Result<T, E>) -> Result<T, E> {
        let started = self.base.now();
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

    /// The file the database lends to the buckets, queues and indexes opened
    /// from it.
    #[cfg(any(feature = "kv", feature = "jobs", feature = "search"))]
    pub(crate) fn shared(&self) -> &Arc<SharedFile> {
        &self.base.shared
    }

    /// `exec` without waiting: `done` is called on the thread that commits it.
    #[cfg(feature = "wire")]
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

    /// `batch` without waiting: `done` is called on the thread that commits it.
    #[cfg(feature = "wire")]
    pub(crate) fn batch_then(&self, statements: Vec<Sql>, done: impl FnOnce(Result<Vec<Done>>) + Send + 'static) {
        #[cfg(feature = "replication")]
        if self.base.file().is_copy() {
            let db = self.clone();
            std::thread::spawn(move || done(db.batch(statements)));
            return;
        }
        let (weight, write) = batch_write(statements);
        let describe = self.base.describe();
        self.base.file().submit(weight, write, move |ran| done(ran.map_err(|error| error.within(describe))));
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
        let fail = self.failed_by(&statement);
        if !self.base.writes(statement.text()) {
            let held = Held::of(&self.base.memory);
            match self.base.file().read(|connection| run::read(connection, &statement, wanted, held)) {
                Ok(Read::Rows(rows)) => return done(Ok(rows)),
                Ok(Read::Writes) => self.base.remember_write(statement.text()),
                Err(error) => return done(Err(fail(error))),
            }
        }
        let (weight, held) = (statement.weight(), Held::of(&self.base.memory));
        let write = move |tx: &crate::sqlite::Tx<'_>| run::write(tx, &statement, wanted, held);
        self.base.file().submit(weight, write, move |rows| done(rows.map_err(fail)));
    }

    /// A statement's rows from a reader alone, for a connection that may not
    /// write: none when SQLite says the statement writes, or when it is not a
    /// query, since SQLite counts an ATTACH, which opens a file and stays on
    /// the shared reader, and a PRAGMA that changes the reader, as reads.
    #[cfg(feature = "wire")]
    pub(crate) fn read<A: Answer>(&self, statement: &Sql, wanted: Wanted) -> Result<Option<A>> {
        if !is_query(statement.text()) || self.base.writes(statement.text()) {
            return Ok(None);
        }
        let held = Held::of(&self.base.memory);
        let read = self.base.file().read(|connection| run::read(connection, statement, wanted, held));
        match read.map_err(|error| self.base.failed(statement, error))? {
            Read::Rows(rows) => Ok(Some(rows)),
            Read::Writes => {
                self.base.remember_write(statement.text());
                Ok(None)
            }
        }
    }

    /// A statement's rows from a reader, or from the writer when SQLite says
    /// it writes; a statement known to write goes to the writer at once.
    fn rows(&self, statement: &Sql, wanted: Wanted) -> Result<Rows> {
        if !self.base.writes(statement.text()) {
            let held = Held::of(&self.base.memory);
            match self.base.file().read(|connection| run::read(connection, statement, wanted, held))? {
                Read::Rows(rows) => return Ok(rows),
                Read::Writes => self.base.remember_write(statement.text()),
            }
        }
        let (running, held) = (statement.clone(), Held::of(&self.base.memory));
        self.base.file().write(statement.weight(), move |tx| run::write(tx, &running, wanted, held))
    }

    /// What names a failure of `statement`, to say it on another thread.
    #[cfg(feature = "wire")]
    fn failed_by(&self, statement: &Sql) -> impl FnOnce(Error) -> Error + Send + 'static {
        let (statement, database) = (statement.describe(), self.base.describe());
        move |error| error.within(statement).within(database)
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
    if crate::text::is_name(name) {
        return Ok(());
    }
    Err(Error::invalid("a database's name is a-z, 0-9, _ and -, at most 64, and starts with a letter or a digit"))
}

/// What a batch weighs in its commit, and its write: every statement in
/// order, a failure naming its own.
fn batch_write(
    statements: Vec<Sql>,
) -> (usize, impl FnOnce(&crate::sqlite::Tx<'_>) -> Result<Vec<Done>> + Send + 'static) {
    let weight = statements.iter().map(Sql::weight).sum();
    let write = move |tx: &crate::sqlite::Tx<'_>| {
        let exec = |statement| run::exec(tx, statement).map_err(|error| error.within(statement.describe()));
        statements.iter().map(exec).collect()
    };
    (weight, write)
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
