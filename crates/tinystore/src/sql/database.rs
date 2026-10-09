use std::fmt;
use std::sync::Arc;

use serde::de::DeserializeOwned;

use super::engine::{Base, Databases};
use super::migrations::Migrations;
use super::rows::Rows;
use super::run::{self, Read, Wanted};
use super::statement::Sql;
use super::tx::Tx;
use crate::{Error, Result, Store};

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
        DatabaseBuilder { store: self.clone(), name: name.to_owned(), migrations: None }
    }
}

/// How a database opens; [`Store::database`] makes one.
#[derive(Debug)]
#[must_use = "a database opens with `open`"]
pub struct DatabaseBuilder {
    store: Store,
    name: String,
    migrations: Option<Migrations>,
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

    pub fn open(self) -> Result<Database> {
        let describe = format!("sql {}", self.name);
        let opened = check_name(&self.name)
            .and_then(|()| self.migrations.map(Migrations::load).transpose())
            .and_then(|migrations| Databases::of(&self.store)?.open(&self.store, &self.name, migrations));
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

impl Database {
    pub fn name(&self) -> &str {
        &self.base.name
    }

    /// The rows a statement gives, each read as a `T`: a struct by its
    /// columns' names, a tuple by their order, a single value when a row has
    /// one column. A write with `returning` runs on the writer and gives its
    /// rows once its commit is durable.
    pub fn all<T: DeserializeOwned>(&self, statement: impl Into<Sql>) -> Result<Vec<T>> {
        let statement = statement.into();
        self.rows(&statement, Wanted::All).and_then(Rows::decode).map_err(|error| self.failed(&statement, error))
    }

    /// The one row a statement gives, or none; a statement that gives two is
    /// `invalid`, and a write that gave them rolls back.
    pub fn one<T: DeserializeOwned>(&self, statement: impl Into<Sql>) -> Result<Option<T>> {
        let statement = statement.into();
        let rows = self.rows(&statement, Wanted::One).and_then(Rows::decode);
        rows.map(|rows: Vec<T>| rows.into_iter().next()).map_err(|error| self.failed(&statement, error))
    }

    /// The one value of the one row a statement gives, as `count(*)` does;
    /// `Option` takes a `NULL`, as `max` over no rows gives.
    pub fn scalar<T: DeserializeOwned>(&self, statement: impl Into<Sql>) -> Result<T> {
        let statement = statement.into();
        self.rows(&statement, Wanted::Scalar).and_then(Rows::scalar).map_err(|error| self.failed(&statement, error))
    }

    /// Runs a write once it is durable. Writes from many callers share a
    /// commit, each in a savepoint of it: one that fails rolls back alone.
    pub fn exec(&self, statement: impl Into<Sql>) -> Result<Done> {
        let statement = statement.into();
        let weight = statement.weight();
        let running = statement.clone();
        self.base.file.write(weight, move |tx| run::exec(tx, &running)).map_err(|error| self.failed(&statement, error))
    }

    /// Runs statements known before they run as one, all of them or none, in
    /// one savepoint of the next shared commit: it costs what one write does.
    pub fn batch<I>(&self, statements: I) -> Result<Vec<Done>>
    where
        I: IntoIterator,
        I::Item: Into<Sql>,
    {
        let statements: Vec<Sql> = statements.into_iter().map(Into::into).collect();
        let weight = statements.iter().map(Sql::weight).sum();
        let ran = self.base.file.write(weight, move |tx| {
            statements
                .iter()
                .map(|statement| run::exec(tx, statement).map_err(|error| error.within(statement.describe())))
                .collect()
        });
        ran.map_err(|error| error.within(self.base.describe()))
    }

    /// Runs `work` in a transaction that holds the writer, for writes that
    /// depend on what its reads found: `Ok` commits, an error or a panic rolls
    /// back. It runs once. Past five seconds it rolls back and is `limit`.
    pub fn tx<T, E: From<Error>>(&self, work: impl FnOnce(&Tx<'_>) -> Result<T, E>) -> Result<T, E> {
        let started = self.base.now();
        self.base.file.transaction(|raw| {
            let tx = Tx::new(raw, &self.base, started);
            let value = work(&tx)?;
            tx.within_bound()?;
            Ok(value)
        })
    }

    /// A statement's rows as SQLite keeps them, for a caller that is not
    /// Rust: the wire's.
    pub(crate) fn rows_of(&self, statement: &Sql, wanted: Wanted) -> Result<Rows> {
        self.rows(statement, wanted).map_err(|error| self.failed(statement, error))
    }

    /// `exec` without waiting: `done` is called on the thread that commits it.
    pub(crate) fn exec_then(&self, statement: Sql, done: impl FnOnce(Result<Done>) + Send + 'static) {
        let (weight, describe) = (statement.weight(), self.failed_by(&statement));
        self.base.file.submit(weight, move |tx| run::exec(tx, &statement), move |done_| done(done_.map_err(describe)));
    }

    /// `batch` without waiting: `done` is called on the thread that commits it.
    pub(crate) fn batch_then(&self, statements: Vec<Sql>, done: impl FnOnce(Result<Vec<Done>>) + Send + 'static) {
        let weight = statements.iter().map(Sql::weight).sum();
        let describe = self.base.describe();
        let write = move |tx: &crate::sqlite::Tx<'_>| -> Result<Vec<Done>> {
            statements
                .iter()
                .map(|statement| run::exec(tx, statement).map_err(|error| error.within(statement.describe())))
                .collect()
        };
        self.base.file.submit(weight, write, move |ran| done(ran.map_err(|error| error.within(describe))));
    }

    /// A statement's rows from a reader, or from the writer when SQLite says
    /// it writes; a statement known to write goes to the writer at once.
    fn rows(&self, statement: &Sql, wanted: Wanted) -> Result<Rows> {
        if !self.base.writes(statement.text()) {
            match self.base.file.read(|connection| run::read(connection, statement, wanted))? {
                Read::Rows(rows) => return Ok(rows),
                Read::Writes => self.base.remember_write(statement.text()),
            }
        }
        let running = statement.clone();
        self.base.file.write(statement.weight(), move |tx| run::write(tx, &running, wanted))
    }

    fn failed(&self, statement: &Sql, error: Error) -> Error {
        error.within(statement.describe()).within(self.base.describe())
    }

    /// What names a failure of `statement`, to say it on another thread.
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
    let mut chars = name.chars();
    let first = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if first && rest && name.len() <= 64 {
        return Ok(());
    }
    Err(Error::invalid("a database's name is a-z, 0-9, _ and -, at most 64, and starts with a letter or a digit"))
}
