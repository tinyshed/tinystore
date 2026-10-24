use std::fmt;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use super::oneshot::pending;
use crate::pool::Pool;
use crate::{Error, Options, Result};

/// The line the store's own transactions wait in on the pool, as a
/// database's wait in `sql <name>`: each holds kv.db's or jobs.db's writer,
/// and one waiting on a thread of its own for the writer another holds would
/// take a thread every other call needs.
#[cfg(any(feature = "kv", feature = "jobs"))]
const TRANSACTIONS: &str = "store";

/// A store whose calls are awaited: the same directory, engines and files as
/// [`crate::Store`], and the same store when one is made from the other.
///
/// ```no_run
/// # async fn example() -> tinystore::Result<()> {
/// use tinystore::Options;
/// use tinystore::aio::Store;
///
/// let store = Store::open("data", Options::default())?;
/// let stock = store.blocking().bucket::<i64>("stock").open()?;
/// let placed = store
///     .tx(move |tx| -> tinystore::Result<bool> {
///         let left = tx.with(&stock).get("sku-1")?.unwrap_or(0);
///         if left < 1 {
///             return Ok(false); // sold out: nothing written
///         }
///         tx.with(&stock).set("sku-1", &(left - 1))?;
///         Ok(true)
///     })
///     .await?;
/// store.close().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Store {
    store: crate::Store,
    pool: Arc<Pool>,
}

impl Store {
    /// Opens the store in `dir` as [`crate::Store::open`] does: a plain call,
    /// made once at start.
    pub fn open(dir: impl AsRef<Path>, options: Options) -> Result<Store> {
        crate::Store::open(dir, options).map(Store::from)
    }

    /// Opens the store in `dir` to read beside the process that holds it, as
    /// [`crate::Store::open_beside`] does.
    pub fn open_beside(dir: impl AsRef<Path>) -> Result<Store> {
        crate::Store::open_beside(dir).map(Store::from)
    }

    /// The same store, for its blocking calls: a command-line tool and a
    /// server beside it share one store, its engines and its files.
    pub fn blocking(&self) -> &crate::Store {
        &self.store
    }

    pub fn dir(&self) -> &Path {
        self.store.dir()
    }

    pub fn now(&self) -> SystemTime {
        self.store.now()
    }

    /// Closes the store as [`crate::Store::close`] does, on a thread the call
    /// starts, so that the executor's thread waits for nothing: the close
    /// waits for background work, for the handlers running and for each
    /// engine's last writes. A store whose last handle is dropped closes on
    /// the dropping thread instead, which may be the executor's.
    pub fn close(&self) -> impl Future<Output = Result<()>> + Send + 'static {
        let store = self.store.clone();
        async move {
            if crate::sqlite::in_a_transaction() {
                let why = "a close from inside a transaction would wait for the writer the transaction holds";
                return Err(Error::invalid(format!("{store:?}: {why}: close after it")));
            }
            let (answer, closed) = pending(format!("{store:?}: its close"));
            // A thread of the pool would wait for the pool it runs on to stop.
            std::thread::Builder::new()
                .name("tinystore-close".to_owned())
                .spawn(move || answer.send(store.close()))
                .map_err(|error| Error::io("a store's close: its thread", error))?;
            closed.await
        }
    }

    /// Runs `work` in one transaction of one of the store's own files, as
    /// [`crate::Store::tx`] does, on a thread of the store's pool: `work` is
    /// blocking and holds the file's writer, so it must not wait for the
    /// program's awaits, and it runs on no executor's thread. Inside, the
    /// calls of a handle go through `tx.with(&handle)` and are blocking.
    ///
    /// Once polled it runs to its end, as a write does, whether or not anyone
    /// still awaits it. A panic in `work` rolls the transaction back and is
    /// resumed at the `.await`.
    #[cfg(any(feature = "kv", feature = "jobs"))]
    pub fn tx<T, E, F>(&self, work: F) -> impl Future<Output = Result<T, E>> + Send + 'static
    where
        F: FnOnce(&crate::Tx<'_>) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<Error> + Send + 'static,
    {
        let (store, what) = (self.store.clone(), format!("{:?}: a transaction", self.store));
        let ran = super::run::blocking(&self.pool, Some(TRANSACTIONS.to_owned()), what, move || Ok(store.tx(work)));
        async move { ran.await.unwrap_or_else(|error| Err(E::from(error))) }
    }

    /// The store's pool, where the calls of every handle opened from it run
    /// what must not run on the executor's thread.
    #[allow(dead_code, reason = "the engines' async handles take the pool from here")]
    pub(crate) fn pool(&self) -> &Arc<Pool> {
        &self.pool
    }
}

impl From<crate::Store> for Store {
    /// The same store, its calls awaited: a clone of its handle, never a
    /// second store.
    fn from(store: crate::Store) -> Store {
        let pool = Pool::of_or_stopped(&store);
        Store { store, pool }
    }
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.store, f)
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
