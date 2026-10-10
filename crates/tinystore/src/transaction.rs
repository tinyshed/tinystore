//! A transaction of one file as the handles it takes in see it. kv's
//! `store.tx` and a database's `db.tx` each run one, and `tx.with(&handle)`
//! runs a bucket's or a queue's calls in it, so that what they write commits
//! with the rest or not at all.

use std::cell::RefCell;
use std::fmt;

use crate::sqlite::{File, Tx as SqlTx, sql_error};
use crate::{Error, Result};

const SAVEPOINT: &str = "savepoint call";
const RELEASE: &str = "release call";
const ROLLBACK_TO: &str = "rollback to call";

/// A transaction of one file, as the handles it takes in see it: the writer it
/// holds, the file a handle must be kept in, the bound it ends within, and
/// what runs once it ends.
pub struct Transaction<'t> {
    raw: &'t SqlTx<'t>,
    file: &'t File,
    /// What an error calls the transaction's file: `kv.db`, `sql app`.
    owner: &'t str,
    bound: Option<&'t dyn Fn() -> Result<()>>,
    #[cfg_attr(not(feature = "jobs"), expect(dead_code, reason = "only a queue asks for what runs once it ends"))]
    ended: &'t Ended,
}

impl Transaction<'_> {
    /// Runs `work` in a transaction of `file` that holds its writer alone,
    /// then what the calls inside asked to run once it ended. A bound, when
    /// there is one, fails every call past it and the commit.
    pub(crate) fn run<T, E: From<Error>>(
        file: &File,
        owner: &str,
        bound: Option<&dyn Fn() -> Result<()>>,
        work: impl FnOnce(&Transaction<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        let ended = Ended::default();
        let outcome = file.transaction(|raw| {
            let transaction = Transaction { raw, file, owner, bound, ended: &ended };
            let value = work(&transaction)?;
            transaction.within_bound()?;
            Ok(value)
        });
        ended.finish(outcome.is_ok());
        outcome
    }

    /// Runs one call in a savepoint of the transaction, rolled back when the
    /// call fails, once the transaction is still within its bound: a call that
    /// fails leaves the transaction as it was before the call.
    pub(crate) fn call<T>(&self, work: impl FnOnce(&SqlTx<'_>) -> Result<T>) -> Result<T> {
        self.within_bound()?;
        let failed = |error| sql_error("a call's savepoint", error);
        self.raw.execute_batch(SAVEPOINT).map_err(failed)?;
        match work(self.raw) {
            Ok(value) => self.raw.execute_batch(RELEASE).map_err(failed).map(|()| value),
            Err(error) => {
                let undone = self.raw.execute_batch(ROLLBACK_TO).and_then(|()| self.raw.execute_batch(RELEASE));
                undone.map_err(failed)?;
                Err(error)
            }
        }
    }

    /// Refuses a handle kept in another file than the transaction's, `place`:
    /// what it wrote would not commit with the rest.
    pub(crate) fn takes(&self, file: &File, place: &str) -> Result<()> {
        if std::ptr::eq(self.file, file) {
            return Ok(());
        }
        Err(Error::invalid(format!("kept in {place}, outside this transaction of {}", self.owner)))
    }

    /// Runs `hook` once the transaction ends, told whether it committed: a
    /// queue wakes its workers only for jobs that are there to find.
    #[cfg(feature = "jobs")]
    pub(crate) fn after(&self, hook: impl FnOnce(bool) + 'static) {
        self.ended.0.borrow_mut().push(Box::new(hook));
    }

    pub(crate) fn within_bound(&self) -> Result<()> {
        self.bound.map_or(Ok(()), |bound| bound())
    }
}

impl fmt::Debug for Transaction<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Transaction({})", self.owner)
    }
}

/// What runs once a transaction ends, told whether it committed.
type Hook = Box<dyn FnOnce(bool)>;

/// What runs once a transaction ends. A transaction that panicked tells it
/// that it did not commit, as it rolls back.
#[derive(Default)]
struct Ended(RefCell<Vec<Hook>>);

impl Ended {
    fn finish(&self, committed: bool) {
        let hooks = std::mem::take(&mut *self.0.borrow_mut());
        for hook in hooks {
            hook(committed);
        }
    }
}

impl Drop for Ended {
    fn drop(&mut self) {
        self.finish(false);
    }
}

/// A handle a transaction of its file takes in with `tx.with(&handle)`: a
/// bucket or counters, and a queue, from the store or from a database.
pub trait TxHandle: sealed::Sealed {
    /// The handle's calls inside a transaction.
    type InTx<'t>
    where
        Self: 't;

    #[doc(hidden)]
    fn in_tx<'t>(&'t self, tx: &'t Transaction<'t>) -> Self::InTx<'t>;
}

/// A handle held by reference joins as the handle does.
impl<H: TxHandle + ?Sized> TxHandle for &H {
    type InTx<'t>
        = H::InTx<'t>
    where
        Self: 't;

    fn in_tx<'t>(&'t self, tx: &'t Transaction<'t>) -> H::InTx<'t> {
        (**self).in_tx(tx)
    }
}

/// Sealed: a public bound that no other crate can implement. Each engine
/// seals its own handles.
pub(crate) mod sealed {
    pub trait Sealed {}

    impl<H: Sealed + ?Sized> Sealed for &H {}
}
