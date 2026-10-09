//! A transaction of kv.db, for reads that decide what to write.
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

use std::fmt;
use std::sync::atomic::AtomicI64;
use std::time::Duration;

use super::bucket::{Bucket, Entry, PAGE_KEYS, Page, clear_work};
use super::counters::Counters;
use super::engine::Kv;
use super::key_call::KeyCall;
use super::path::Key;
use super::scope::Scope;
use super::value::Value;
use crate::sqlite::{Tx as SqlTx, sql_error};
use crate::{Error, Result, Store};

impl Store {
    /// Runs `work` in one transaction of kv.db that holds its writer alone, so
    /// that what it reads cannot change before it writes: two orders cannot
    /// both take the last item. It commits when `work` returns `Ok` and rolls
    /// back when it returns an error or panics.
    ///
    /// Inside, every call goes through the transaction, `tx.with(&bucket)`. A
    /// call on kv.db made around it from inside would not see what it wrote,
    /// or would wait for the writer it holds: it fails `Invalid` instead.
    /// Writes that need no reads need no transaction, since separate calls
    /// already share commits.
    pub fn tx<T, E: From<Error>>(&self, work: impl FnOnce(&Tx<'_>) -> Result<T, E>) -> Result<T, E> {
        let kv = Kv::of(self).map_err(|error| E::from(error.within("kv tx")))?;
        kv.file().transaction(|sql| work(&Tx { kv: &kv, sql }))
    }
}

/// A transaction of kv.db, given to the function `Store::tx` runs.
pub struct Tx<'a> {
    kv: &'a Kv,
    sql: &'a SqlTx<'a>,
}

impl<'a> Tx<'a> {
    /// The calls of `handle` inside this transaction: what they read sees what
    /// the transaction wrote, and what they write commits with it or not at
    /// all. A handle of another store is refused at its first call.
    pub fn with<'h, H: TxHandle>(&'h self, handle: &'h H) -> H::InTx<'h> {
        handle.in_tx(self)
    }

    /// Runs one call of a handle in a savepoint of the transaction, so that a
    /// call that fails leaves the transaction as it was before the call.
    pub(crate) fn run<T>(
        &self,
        scope: &Scope,
        key: &str,
        work: impl FnOnce(&SqlTx<'_>, &AtomicI64) -> Result<T>,
    ) -> Result<T> {
        self.check(scope)?;
        let revision = self.kv.revision();
        savepoint(self.sql, || work(self.sql, &revision)).map_err(|error| error.within(scope.shown_key(key)))
    }

    fn check(&self, scope: &Scope) -> Result<()> {
        if std::ptr::eq(&*scope.kv, self.kv) {
            return Ok(());
        }
        Err(Error::invalid("a handle of another store, in a transaction of this one").within(scope.shown()))
    }
}

impl fmt::Debug for Tx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Tx({:?})", self.kv)
    }
}

/// A handle a transaction takes in with `tx.with(&handle)`: a bucket or
/// counters kept in the file.
pub trait TxHandle: sealed::Sealed {
    /// The handle's calls inside a transaction.
    type InTx<'h>
    where
        Self: 'h;

    #[doc(hidden)]
    fn in_tx<'h>(&'h self, tx: &'h Tx<'h>) -> Self::InTx<'h>;
}

/// Sealed: a public bound that no other crate can implement.
mod sealed {
    pub trait Sealed {}

    impl<V> Sealed for super::Bucket<V> {}
    impl Sealed for super::Counters {}
    impl<H: Sealed + ?Sized> Sealed for &H {}
}

/// A handle held by reference joins as the handle does.
impl<H: TxHandle + ?Sized> TxHandle for &H {
    type InTx<'h>
        = H::InTx<'h>
    where
        Self: 'h;

    fn in_tx<'h>(&'h self, tx: &'h Tx<'h>) -> H::InTx<'h> {
        (**self).in_tx(tx)
    }
}

impl<V: Value> TxHandle for Bucket<V> {
    type InTx<'h>
        = TxBucket<'h, V>
    where
        Self: 'h;

    fn in_tx<'h>(&'h self, tx: &'h Tx<'h>) -> TxBucket<'h, V> {
        TxBucket { bucket: self, tx }
    }
}

impl TxHandle for Counters {
    type InTx<'h> = TxCounters<'h>;

    fn in_tx<'h>(&'h self, tx: &'h Tx<'h>) -> TxCounters<'h> {
        TxCounters { counters: self, tx }
    }
}

/// A bucket's calls inside a transaction.
pub struct TxBucket<'a, V> {
    bucket: &'a Bucket<V>,
    tx: &'a Tx<'a>,
}

impl<'a, V: Value> TxBucket<'a, V> {
    pub fn get(&self, key: impl Key) -> Result<Option<V>> {
        Ok(self.entry(key)?.map(|entry| entry.value))
    }

    pub fn entry(&self, key: impl Key) -> Result<Option<Entry<V>>> {
        let key = key.text();
        let cell = self.tx.run(&self.bucket.scope, &key, |sql, _| self.bucket.read_cell_in(sql, &key))?;
        cell.map(|cell| self.bucket.entry_of(&key, cell)).transpose()
    }

    pub fn has(&self, key: impl Key) -> Result<bool> {
        let key = key.text();
        Ok(self.tx.run(&self.bucket.scope, &key, |sql, _| self.bucket.read_cell_in(sql, &key))?.is_some())
    }

    pub fn set(&self, key: impl Key, value: &V) -> Result<()> {
        self.key(key).set(value)
    }

    pub fn create(&self, key: impl Key, value: &V) -> Result<bool> {
        self.key(key).create(value)
    }

    /// Takes `key`; a value that no longer reads as the bucket's type is
    /// `Corrupt`, and stays, and the transaction goes on.
    pub fn take(&self, key: impl Key) -> Result<Option<V>> {
        self.key(key).take()
    }

    pub fn delete(&self, key: impl Key) -> Result<bool> {
        self.key(key).delete()
    }

    pub fn expire(&self, key: impl Key, ttl: Duration) -> Result<bool> {
        self.key(key).ttl(ttl).expire()
    }

    /// A call on `key` with what it asks beside its value, inside the
    /// transaction: `tx.with(&sessions).key(&token).if_version(seen).set(&next)`.
    pub fn key(&self, key: impl Key) -> KeyCall<'a, V> {
        KeyCall::new(self.bucket, Some(self.tx), key.text().into_owned())
    }

    /// Removes every key of the bucket's branch and of the branches under it.
    pub fn clear(&self) -> Result<()> {
        let work = clear_work(&self.bucket.scope)?;
        let shown = self.bucket.scope.shown_branch();
        self.tx.check(&self.bucket.scope)?;
        let revision = self.tx.kv.revision();
        savepoint(self.tx.sql, || work(self.tx.sql, &revision)).map_err(|error| error.within(shown))
    }

    /// One page of the branch's own keys, as `Bucket::list` reads it.
    pub fn list(&self, limit: usize, after: Option<&str>) -> Result<Page<V>> {
        let limit = limit.clamp(1, PAGE_KEYS);
        self.tx.check(&self.bucket.scope)?;
        let rows = self
            .bucket
            .page_rows(self.tx.sql, limit, after)
            .map_err(|error| error.within(self.bucket.scope.shown_branch()))?;
        self.bucket.page_of(rows, limit)
    }
}

impl<V> fmt::Debug for TxBucket<'_, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TxBucket({:?})", self.bucket.scope)
    }
}

/// Counters' calls inside a transaction. Counters kept in memory join no
/// transaction: their calls through it are `Invalid`.
pub struct TxCounters<'a> {
    counters: &'a Counters,
    tx: &'a Tx<'a>,
}

impl TxCounters<'_> {
    pub fn add(&self, key: impl Key, n: i64) -> Result<i64> {
        let key = key.text();
        self.refuse_in_memory()?;
        let work = self.counters.add_work(&key, n)?;
        self.tx.run(&self.counters.scope, &key, work)
    }

    pub fn get(&self, key: impl Key) -> Result<i64> {
        let key = key.text();
        self.refuse_in_memory()?;
        self.tx.run(&self.counters.scope, &key, |sql, _| self.counters.get_on(sql, &key))
    }

    pub fn delete(&self, key: impl Key) -> Result<bool> {
        let key = key.text();
        self.refuse_in_memory()?;
        let work = self.counters.delete_work(&key)?;
        self.tx.run(&self.counters.scope, &key, work)
    }

    fn refuse_in_memory(&self) -> Result<()> {
        if self.counters.buffer.is_none() {
            return Ok(());
        }
        Err(Error::invalid("counters kept in memory join no transaction; open them without flush_every to")
            .within(self.counters.scope.shown()))
    }
}

impl fmt::Debug for TxCounters<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TxCounters({:?})", self.counters.scope)
    }
}

const SAVEPOINT: &str = "savepoint call";
const RELEASE: &str = "release call";
const ROLLBACK_TO: &str = "rollback to call";

/// Runs `work` in a savepoint of the transaction, kept when it succeeds and
/// rolled back when it fails.
fn savepoint<T>(sql: &SqlTx<'_>, work: impl FnOnce() -> Result<T>) -> Result<T> {
    let fail = |error| sql_error("a call's savepoint", error);
    sql.execute_batch(SAVEPOINT).map_err(fail)?;
    match work() {
        Ok(value) => sql.execute_batch(RELEASE).map_err(fail).map(|()| value),
        Err(error) => {
            sql.execute_batch(ROLLBACK_TO).and_then(|()| sql.execute_batch(RELEASE)).map_err(fail)?;
            Err(error)
        }
    }
}

#[cfg(test)]
#[path = "tx_tests.rs"]
mod tests;
