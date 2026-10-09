use std::time::{Duration, SystemTime};

use serde::de::DeserializeOwned;

use super::database::Done;
use super::engine::Base;
use super::rows::Rows;
use super::run::{self, Wanted};
use super::statement::Sql;
use crate::sqlite::{execute, sql_error};
use crate::{Error, Result};

/// How long a transaction holds the writer at most: past it, it rolls back.
pub(crate) const BOUND: Duration = Duration::from_secs(5);

const SAVEPOINT: &str = "savepoint call";
const RELEASE: &str = "release call";
const ROLLBACK_TO: &str = "rollback to call";

/// A database's transaction, from [`Database::tx`](super::Database::tx): its
/// reads see its writes, and a call that fails leaves it as it was before the
/// call.
pub struct Tx<'t> {
    raw: &'t crate::sqlite::Tx<'t>,
    base: &'t Base,
    started: SystemTime,
}

impl<'t> Tx<'t> {
    pub(crate) fn new(raw: &'t crate::sqlite::Tx<'t>, base: &'t Base, started: SystemTime) -> Self {
        Tx { raw, base, started }
    }

    pub fn all<T: DeserializeOwned>(&self, statement: impl Into<Sql>) -> Result<Vec<T>> {
        let statement = statement.into();
        self.rows(&statement, Wanted::All).and_then(Rows::decode).map_err(|error| self.failed(&statement, error))
    }

    pub fn one<T: DeserializeOwned>(&self, statement: impl Into<Sql>) -> Result<Option<T>> {
        let statement = statement.into();
        let rows = self.rows(&statement, Wanted::One).and_then(Rows::decode);
        rows.map(|rows: Vec<T>| rows.into_iter().next()).map_err(|error| self.failed(&statement, error))
    }

    pub fn scalar<T: DeserializeOwned>(&self, statement: impl Into<Sql>) -> Result<T> {
        let statement = statement.into();
        self.rows(&statement, Wanted::Scalar).and_then(Rows::scalar).map_err(|error| self.failed(&statement, error))
    }

    pub fn exec(&self, statement: impl Into<Sql>) -> Result<Done> {
        let statement = statement.into();
        self.call(|| run::exec(self.raw, &statement)).map_err(|error| self.failed(&statement, error))
    }

    /// A statement's rows as SQLite keeps them, for the wire.
    pub(crate) fn rows_of(&self, statement: &Sql, wanted: Wanted) -> Result<Rows> {
        self.rows(statement, wanted).map_err(|error| self.failed(statement, error))
    }

    fn rows(&self, statement: &Sql, wanted: Wanted) -> Result<Rows> {
        self.call(|| run::write(self.raw, statement, wanted))
    }

    /// Runs one call in a savepoint of the transaction, rolled back when the
    /// call fails, once the transaction is still within its bound.
    fn call<T>(&self, work: impl FnOnce() -> Result<T>) -> Result<T> {
        self.within_bound()?;
        let what = "a call: its savepoint";
        execute(self.raw, SAVEPOINT).map_err(|error| sql_error(what, error))?;
        let outcome = work();
        let ended = match outcome {
            Ok(_) => execute(self.raw, RELEASE),
            Err(_) => execute(self.raw, ROLLBACK_TO).and_then(|()| execute(self.raw, RELEASE)),
        };
        ended.map_err(|error| sql_error(what, error))?;
        outcome
    }

    /// Fails once the transaction has held the writer past its bound, which
    /// then rolls back.
    pub(crate) fn within_bound(&self) -> Result<()> {
        let held = self.base.now().duration_since(self.started).unwrap_or_default();
        if held <= BOUND {
            return Ok(());
        }
        Err(Error::limit(format!(
            "{}: a transaction held the writer {held:?}, past its {BOUND:?}, and rolls back",
            self.base.describe()
        )))
    }

    fn failed(&self, statement: &Sql, error: Error) -> Error {
        error.within(statement.describe()).within(self.base.describe())
    }
}

impl std::fmt::Debug for Tx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tx({})", self.base.name)
    }
}
