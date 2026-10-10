//! Transactions as the handles they take in see them. The store's `store.tx`
//! and a database's `db.tx` each run one, and `tx.with(&handle)` runs a
//! bucket's or a queue's calls in it, so that what they write commits with the
//! rest or not at all.
//!
//! ```no_run
//! # fn main() -> tinystore::Result<()> {
//! # let store = tinystore::Store::open("data", Default::default())?;
//! let stock = store.bucket::<i64>("stock").open()?;
//! let orders = store.bucket::<String>("orders").open()?;
//! let placed = store.tx(|tx| -> tinystore::Result<bool> {
//!     let left = tx.with(&stock).get("sku-1")?.unwrap_or(0);
//!     if left < 1 {
//!         return Ok(false); // sold out: nothing written
//!     }
//!     tx.with(&stock).set("sku-1", &(left - 1))?;
//!     tx.with(&orders).set("order-7", &"sku-1".to_owned())?;
//!     Ok(true)
//! })?;
//! # Ok(())
//! # }
//! ```

use std::cell::RefCell;
use std::fmt;
use std::sync::Arc;

use crate::sqlite::{Begun, File, Mark, Tx as SqlTx, sql_error};
use crate::{Error, Result, Store};

const SAVEPOINT: &str = "savepoint call";
const RELEASE: &str = "release call";
const ROLLBACK_TO: &str = "rollback to call";

impl Store {
    /// Runs `work` in one transaction of one of the store's own files, kv.db
    /// or jobs.db: the file of the first handle `tx.with` takes in, whose
    /// writer it then holds alone, so that what it reads cannot change before
    /// it writes. Two orders cannot both take the last item, and a hundred
    /// jobs are added together or not at all. It commits when `work` returns
    /// `Ok` and rolls back when it returns an error or panics.
    ///
    /// Inside, every call on the store's buckets and queues goes through the
    /// transaction, `tx.with(&bucket)`: one made around it would not see what
    /// it wrote, or would wait for the writer it holds, and fails `Invalid`
    /// instead. A bucket and a queue of the store are two files, and no write
    /// is atomic across two: a handle kept in another file than the first is
    /// `Invalid`, and to commit keys and jobs together a program opens both
    /// from a database. Writes that need no reads need no transaction, since
    /// separate calls already share commits.
    pub fn tx<T, E: From<Error>>(&self, work: impl FnOnce(&Tx<'_>) -> Result<T, E>) -> Result<T, E> {
        self.refuse_when_closed("a transaction")?;
        let files = self.engine_files();
        Transaction::of_store(&files, |transaction| work(&Tx { transaction }))
    }
}

/// A transaction of the store, given to the function [`Store::tx`] runs.
pub struct Tx<'a> {
    transaction: &'a Transaction<'a>,
}

impl Tx<'_> {
    /// The calls of `handle` inside this transaction: what they read sees what
    /// the transaction wrote, and what they write commits with it or not at
    /// all. A handle kept in another file is refused at its first call.
    pub fn with<'h, H: TxHandle>(&'h self, handle: &'h H) -> H::InTx<'h> {
        handle.in_tx(self.transaction)
    }

    /// The transaction itself, for the wire's calls in it.
    #[cfg_attr(not(feature = "jobs"), expect(dead_code, reason = "the wire runs a queue's batch in one"))]
    pub(crate) fn transaction(&self) -> &Transaction<'_> {
        self.transaction
    }
}

impl fmt::Debug for Tx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Tx")
    }
}

/// A transaction as the handles it takes in see it: the writer it holds, the
/// file a handle must be kept in, the bound it ends within, and what runs
/// once it ends.
pub struct Transaction<'t> {
    writer: &'t dyn Writer,
    bound: Option<&'t dyn Fn() -> Result<()>>,
    #[cfg_attr(not(feature = "jobs"), expect(dead_code, reason = "only a queue asks for what runs once it ends"))]
    ended: &'t Ended,
}

impl Transaction<'_> {
    /// Runs `work` in a transaction of `file` that holds its writer alone,
    /// then what the calls inside asked to run once it ended. A bound, when
    /// there is one, fails every call past it and the commit.
    #[cfg(feature = "sql")]
    pub(crate) fn run<T, E: From<Error>>(
        file: &File,
        owner: &str,
        bound: Option<&dyn Fn() -> Result<()>>,
        work: impl FnOnce(&Transaction<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        let ended = Ended::default();
        let outcome = file.transaction(|raw| {
            let writer = OneFile { raw, file, owner };
            let transaction = Transaction { writer: &writer, bound, ended: &ended };
            let value = work(&transaction)?;
            transaction.within_bound()?;
            Ok(value)
        });
        ended.finish(outcome.is_ok());
        outcome
    }

    /// Runs `work` in a transaction of whichever of the store's `files` its
    /// first handle is kept in, begun when that handle makes its first call.
    /// The thread is marked for all of them from the start, so that a call
    /// around the transaction is refused whichever it turns out to be of.
    fn of_store<T, E: From<Error>>(
        files: &[Arc<File>],
        work: impl FnOnce(&Transaction<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        let ended = Ended::default();
        let outcome = (|| {
            let marks = files.iter().map(|file| file.mark()).collect::<Result<Vec<_>>>()?;
            let writer = OfStore { files, begun: RefCell::new(None), marks };
            let value = work(&Transaction { writer: &writer, bound: None, ended: &ended })?;
            writer.commit()?;
            Ok(value)
        })();
        ended.finish(outcome.is_ok());
        outcome
    }

    /// Runs one call in a savepoint of the transaction, rolled back when the
    /// call fails, once the transaction is still within its bound: a call that
    /// fails leaves the transaction as it was before the call.
    pub(crate) fn call<T>(&self, work: impl FnOnce(&SqlTx<'_>) -> Result<T>) -> Result<T> {
        self.within_bound()?;
        let (mut work, mut value) = (Some(work), None);
        self.writer.on(&mut |raw| {
            let work = work.take().ok_or_else(|| Error::internal("a transaction's call ran twice"))?;
            value = Some(in_savepoint(raw, || work(raw))?);
            Ok(())
        })?;
        value.ok_or_else(|| Error::internal("a transaction's call did not run"))
    }

    /// Refuses a handle kept in another file than the transaction's, `place`:
    /// what it wrote would not commit with the rest.
    pub(crate) fn takes(&self, file: &File, place: &str) -> Result<()> {
        self.writer.takes(file, place)
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
        f.write_str("Transaction")
    }
}

/// What holds a transaction's writer and says which file it is of.
trait Writer {
    /// Refuses a handle kept in another file than the transaction's.
    fn takes(&self, file: &File, place: &str) -> Result<()>;

    /// Runs `work` on the writer the transaction holds.
    fn on(&self, work: &mut dyn FnMut(&SqlTx<'_>) -> Result<()>) -> Result<()>;
}

/// The writer of a transaction that began on its file before its function
/// ran: a database's.
#[cfg(feature = "sql")]
struct OneFile<'t> {
    raw: &'t SqlTx<'t>,
    file: &'t File,
    /// What an error calls the file: `sql app`.
    owner: &'t str,
}

#[cfg(feature = "sql")]
impl Writer for OneFile<'_> {
    fn takes(&self, file: &File, place: &str) -> Result<()> {
        if std::ptr::eq(self.file, file) {
            return Ok(());
        }
        Err(Error::invalid(format!("kept in {place}, outside this transaction of {}", self.owner)))
    }

    fn on(&self, work: &mut dyn FnMut(&SqlTx<'_>) -> Result<()>) -> Result<()> {
        work(self.raw)
    }
}

/// The writer of the store's transaction, which begins on the file of the
/// first handle it takes in, one of the store's own.
struct OfStore<'f> {
    files: &'f [Arc<File>],
    /// The transaction once it began, and what an error calls its file.
    begun: RefCell<Option<(Begun<'f>, String)>>,
    /// This thread marked inside a transaction of each file, in their order.
    marks: Vec<Mark>,
}

impl OfStore<'_> {
    fn commit(&self) -> Result<()> {
        match self.begun.borrow_mut().take() {
            Some((begun, _)) => begun.commit(),
            None => Ok(()),
        }
    }
}

impl Writer for OfStore<'_> {
    fn takes(&self, file: &File, place: &str) -> Result<()> {
        let mut begun = self.begun.borrow_mut();
        if let Some((held, owner)) = begun.as_ref() {
            if std::ptr::eq(held.file(), file) {
                return Ok(());
            }
            return Err(Error::invalid(format!("kept in {place}, outside this transaction of {owner}")));
        }
        let Some(at) = self.files.iter().position(|own| std::ptr::eq(&**own, file)) else {
            return Err(Error::invalid(format!(
                "kept in {place}, which this store's transaction does not hold: a database's handle joins its db.tx"
            )));
        };
        *begun = Some((self.files[at].begin(&self.marks[at])?, place.to_owned()));
        Ok(())
    }

    fn on(&self, work: &mut dyn FnMut(&SqlTx<'_>) -> Result<()>) -> Result<()> {
        let begun = self.begun.borrow();
        let Some((begun, _)) = begun.as_ref() else {
            return Err(Error::internal("a call in a transaction that took no handle in"));
        };
        work(&begun.tx())
    }
}

/// Runs `work` in a savepoint, kept when it succeeds and rolled back when it
/// fails.
fn in_savepoint<T>(raw: &SqlTx<'_>, work: impl FnOnce() -> Result<T>) -> Result<T> {
    let failed = |error| sql_error("a call's savepoint", error);
    raw.execute_batch(SAVEPOINT).map_err(failed)?;
    match work() {
        Ok(value) => raw.execute_batch(RELEASE).map_err(failed).map(|()| value),
        Err(error) => {
            raw.execute_batch(ROLLBACK_TO).and_then(|()| raw.execute_batch(RELEASE)).map_err(failed)?;
            Err(error)
        }
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
