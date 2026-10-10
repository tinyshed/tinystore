//! The calls of a bucket or of counters inside a transaction of their file:
//! the store's, when they are kept in kv.db, or the database's they were
//! opened from.

use std::fmt;
use std::sync::atomic::AtomicI64;
use std::time::Duration;

use super::bucket::{Bucket, Entry, PAGE_KEYS, Page, clear_work};
use super::counters::Counters;
use super::key_call::KeyCall;
use super::path::Key;
use super::scope::Scope;
use super::value::Value;
use crate::sqlite::Tx as SqlTx;
use crate::transaction::sealed::Sealed;
use crate::{Error, Result, Transaction, TxHandle};

/// Runs one call of a handle of `scope` on `key` in a savepoint of the
/// transaction, once the handle is kept in the transaction's file.
pub(crate) fn call_on_key<T>(
    tx: &Transaction<'_>,
    scope: &Scope,
    key: &str,
    work: impl FnOnce(&SqlTx<'_>, &AtomicI64) -> Result<T>,
) -> Result<T> {
    call(tx, scope, work).map_err(|error| error.within(scope.shown_key(key)))
}

/// Runs one call of a handle of `scope` on its whole branch, as
/// [`call_on_key`] runs one on a key.
pub(crate) fn call_on_branch<T>(
    tx: &Transaction<'_>,
    scope: &Scope,
    work: impl FnOnce(&SqlTx<'_>, &AtomicI64) -> Result<T>,
) -> Result<T> {
    call(tx, scope, work).map_err(|error| error.within(scope.shown_branch()))
}

fn call<T>(tx: &Transaction<'_>, scope: &Scope, work: impl FnOnce(&SqlTx<'_>, &AtomicI64) -> Result<T>) -> Result<T> {
    let revision = scope.kv.revision();
    tx.takes(scope.kv.file(), scope.kv.place())?;
    tx.call(|sql| work(sql, &revision))
}

impl<V> Sealed for Bucket<V> {}
impl Sealed for Counters {}

impl<V: Value> TxHandle for Bucket<V> {
    type InTx<'h>
        = TxBucket<'h, V>
    where
        Self: 'h;

    fn in_tx<'h>(&'h self, tx: &'h Transaction<'h>) -> TxBucket<'h, V> {
        TxBucket { bucket: self, tx }
    }
}

impl TxHandle for Counters {
    type InTx<'h> = TxCounters<'h>;

    fn in_tx<'h>(&'h self, tx: &'h Transaction<'h>) -> TxCounters<'h> {
        TxCounters { counters: self, tx }
    }
}

/// A bucket's calls inside a transaction.
pub struct TxBucket<'a, V> {
    bucket: &'a Bucket<V>,
    tx: &'a Transaction<'a>,
}

impl<'a, V: Value> TxBucket<'a, V> {
    pub fn get(&self, key: impl Key) -> Result<Option<V>> {
        Ok(self.entry(key)?.map(|entry| entry.value))
    }

    pub fn entry(&self, key: impl Key) -> Result<Option<Entry<V>>> {
        let key = key.text();
        let cell = call_on_key(self.tx, &self.bucket.scope, &key, |sql, _| self.bucket.read_cell_in(sql, &key))?;
        cell.map(|cell| self.bucket.entry_of(&key, cell)).transpose()
    }

    pub fn has(&self, key: impl Key) -> Result<bool> {
        let key = key.text();
        Ok(call_on_key(self.tx, &self.bucket.scope, &key, |sql, _| self.bucket.read_cell_in(sql, &key))?.is_some())
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
        let scope = &self.bucket.scope;
        let work = clear_work(scope).map_err(|error| error.within(scope.shown_branch()))?;
        call_on_branch(self.tx, scope, work)
    }

    /// One page of the branch's own keys, as `Bucket::list` reads it.
    pub fn list(&self, limit: usize, after: Option<&str>) -> Result<Page<V>> {
        let limit = limit.clamp(1, PAGE_KEYS);
        let rows = call_on_branch(self.tx, &self.bucket.scope, |sql, _| self.bucket.page_rows(sql, limit, after))?;
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
    tx: &'a Transaction<'a>,
}

impl TxCounters<'_> {
    pub fn add(&self, key: impl Key, n: i64) -> Result<i64> {
        let key = key.text();
        self.refuse_in_memory()?;
        let work = self.counters.add_work(&key, n)?;
        call_on_key(self.tx, &self.counters.scope, &key, work)
    }

    pub fn get(&self, key: impl Key) -> Result<i64> {
        let key = key.text();
        self.refuse_in_memory()?;
        call_on_key(self.tx, &self.counters.scope, &key, |sql, _| self.counters.get_on(sql, &key))
    }

    pub fn delete(&self, key: impl Key) -> Result<bool> {
        let key = key.text();
        self.refuse_in_memory()?;
        let work = self.counters.delete_work(&key)?;
        call_on_key(self.tx, &self.counters.scope, &key, work)
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

#[cfg(test)]
#[path = "tx_tests.rs"]
mod tests;
