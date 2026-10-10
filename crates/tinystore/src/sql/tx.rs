use std::time::Duration;

use serde::de::DeserializeOwned;

use super::database::Done;
use super::engine::Base;
use super::rows::Rows;
use super::run::{self, Wanted};
use super::statement::Sql;
use crate::{Result, Transaction, TxHandle};

/// How long a transaction holds the writer at most: past it, it rolls back.
pub(crate) const BOUND: Duration = Duration::from_secs(5);

/// A database's transaction, from [`Database::tx`](super::Database::tx): its
/// reads see its writes, and a call that fails leaves it as it was before the
/// call.
pub struct Tx<'t> {
    transaction: &'t Transaction<'t>,
    base: &'t Base,
}

impl<'t> Tx<'t> {
    pub(crate) fn new(transaction: &'t Transaction<'t>, base: &'t Base) -> Self {
        Tx { transaction, base }
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
        self.transaction.call(|raw| run::exec(raw, &statement)).map_err(|error| self.failed(&statement, error))
    }

    /// The calls of a bucket or a queue opened from this database, inside the
    /// transaction: what they write commits with the rows or not at all.
    ///
    /// ```no_run
    /// # #[derive(serde::Serialize, serde::Deserialize)]
    /// # struct Email { order: i64 }
    /// # fn main() -> tinystore::Result<()> {
    /// # let store = tinystore::Store::open("data", Default::default())?;
    /// let db = store.database("app").open()?;
    /// let emails = db.queue::<Email>("emails").open()?;
    /// db.tx(|tx| -> tinystore::Result<()> {
    ///     let order: i64 = tx.scalar(tinystore::sql!("insert into orders (total) values (?) returning id", 70))?;
    ///     tx.with(&emails).add(&Email { order })?; // sent only if the order is there
    ///     Ok(())
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn with<'h, H: TxHandle>(&'h self, handle: &'h H) -> H::InTx<'h> {
        handle.in_tx(self.transaction)
    }

    /// A statement's rows as SQLite keeps them, for the wire.
    pub(crate) fn rows_of(&self, statement: &Sql, wanted: Wanted) -> Result<Rows> {
        self.rows(statement, wanted).map_err(|error| self.failed(statement, error))
    }

    /// The transaction itself, for the wire's calls of other engines in it.
    pub(crate) fn transaction(&self) -> &'t Transaction<'t> {
        self.transaction
    }

    fn rows(&self, statement: &Sql, wanted: Wanted) -> Result<Rows> {
        self.transaction.call(|raw| run::write(raw, statement, wanted))
    }

    fn failed(&self, statement: &Sql, error: crate::Error) -> crate::Error {
        error.within(statement.describe()).within(self.base.describe())
    }
}

impl std::fmt::Debug for Tx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tx({})", self.base.name)
    }
}
