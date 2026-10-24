use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use super::named;
use crate::aio::{Store, run};
use crate::cancel;
use crate::kv::{Key, TxCounters};
use crate::pool::Pool;
use crate::transaction::sealed::Sealed;
use crate::{Result, Transaction, TxHandle};

/// Counters being opened, as [`crate::kv::CountersBuilder`] are: their
/// name, how long a counter lasts, and whether they are kept in memory
/// between flushes.
#[must_use = "counters open with open()"]
pub struct CountersBuilder {
    builder: crate::kv::CountersBuilder,
    pool: Arc<Pool>,
}

impl Store {
    /// Numbers by key that add up, in the store's kv.db, whose calls are
    /// awaited.
    pub fn counters(&self, name: &str) -> CountersBuilder {
        CountersBuilder { builder: self.blocking().counters(name), pool: Arc::clone(self.pool()) }
    }
}

impl CountersBuilder {
    /// A counter lasts this long from its first add, then starts again from 0.
    pub fn ttl(self, ttl: Duration) -> Self {
        CountersBuilder { builder: self.builder.ttl(ttl), ..self }
    }

    /// Keeps the counters in memory and writes what changed once a span: an
    /// add is then a step in memory, answered in its first poll.
    pub fn flush_every(self, span: Duration) -> Self {
        CountersBuilder { builder: self.builder.flush_every(span), ..self }
    }

    /// Opens the counters: a plain call, made once at start.
    pub fn open(self) -> Result<Counters> {
        Ok(Counters { counters: self.builder.open()?, pool: self.pool })
    }
}

/// Numbers by key that only add up, their calls awaited: the same counters
/// as [`crate::kv::Counters`].
///
/// What the builder chose decides where a call runs, never the call:
/// counters written each add queue at kv.db's writer, and counters kept in
/// memory step there in the first poll.
///
/// On a node that copies kv.db, an add of counters written each add is queued
/// on that node and answered at once, on the store's pool, and a `get` reads
/// the node's copy there. Every other add, and each `delete` and `clear`,
/// passes on to the file's home and waits for its answer on a thread of its
/// own.
pub struct Counters {
    counters: crate::kv::Counters,
    pool: Arc<Pool>,
}

/// Where a call of counters runs.
#[derive(Clone, Copy)]
enum Runs {
    /// Written each add: queued at the writer.
    Written,
    /// Kept in memory: a step in the poll, or on the pool where it would
    /// read the file or flush first.
    InMemory,
    /// On a copy: passed on to the home and waited for on a thread of its
    /// own, unless it is queued on this node.
    Copy,
}

impl Counters {
    /// Adds `n` to the counter under `key` and returns what it holds now, as
    /// [`crate::kv::Counters::add`] does. Counters kept in memory answer in
    /// the first poll, unless a key memory does not hold yet is read or
    /// memory must flush first: that step runs on the store's pool, and once
    /// polled it counts whether or not its future is still awaited.
    pub fn add(&self, key: impl Key, n: i64) -> impl Future<Output = Result<i64>> + Send + 'static {
        let (counters, key, pool, runs) = (self.counters.clone(), key.text().into_owned(), self.pool(), self.runs());
        let (tokens, what) = (cancel::captured(), named(counters.scope.shown_key(&key), "an add"));
        async move {
            match runs {
                Runs::Written => {
                    run::write(&pool, what, move |answer| counters.add_then(&key, n, move |sum| answer.send(sum))).await
                }
                Runs::InMemory => tokens.run(None, || run::step(&pool, what, move || counters.add(&*key, n))).await,
                // REPL-12: queued on this node's disk, with no wait for the home
                Runs::Copy if counters.queued_on_a_copy() => {
                    run::blocking(&pool, None, what, move || counters.add(&*key, n)).await
                }
                Runs::Copy => run::passed_on(what, move || counters.add(&*key, n)).await,
            }
        }
    }

    /// What the counter under `key` holds; 0 when it is not there.
    pub fn get(&self, key: impl Key) -> impl Future<Output = Result<i64>> + Send + 'static {
        let (counters, key, pool, runs) = (self.counters.clone(), key.text().into_owned(), self.pool(), self.runs());
        let (tokens, what) = (cancel::captured(), named(counters.scope.shown_key(&key), "a get"));
        async move {
            match runs {
                Runs::Written | Runs::InMemory => {
                    tokens.run(None, || run::point(&pool, what, move || counters.get(&*key))).await
                }
                Runs::Copy => tokens.run(None, || run::read(&pool, what, move || counters.get(&*key))).await,
            }
        }
    }

    /// Removes the counter under `key` and says whether it was there.
    pub fn delete(&self, key: impl Key) -> impl Future<Output = Result<bool>> + Send + 'static {
        let (counters, key, pool, runs) = (self.counters.clone(), key.text().into_owned(), self.pool(), self.runs());
        let (tokens, what) = (cancel::captured(), named(counters.scope.shown_key(&key), "a delete"));
        async move {
            match runs {
                Runs::Written => {
                    run::write(&pool, what, move |answer| counters.delete_then(&key, move |found| answer.send(found)))
                        .await
                }
                Runs::InMemory => tokens.run(None, || run::step(&pool, what, move || counters.delete(&*key))).await,
                Runs::Copy => run::passed_on(what, move || counters.delete(&*key)).await,
            }
        }
    }

    /// Removes every counter of this branch and of the branches under it.
    /// Counters kept in memory hold that memory across the clear's commit,
    /// so their clear runs on the store's pool, to its end once polled.
    pub fn clear(&self) -> impl Future<Output = Result<()>> + Send + 'static {
        let (counters, pool, runs) = (self.counters.clone(), self.pool(), self.runs());
        let what = named(counters.scope.shown_branch(), "a clear");
        async move {
            match runs {
                Runs::Written => {
                    run::write(&pool, what, move |answer| counters.clear_then(move |cleared| answer.send(cleared)))
                        .await
                }
                Runs::InMemory => run::blocking(&pool, None, what, move || counters.clear()).await,
                Runs::Copy => run::passed_on(what, move || counters.clear()).await,
            }
        }
    }

    /// The same counters seen from the branch `owner` names under these.
    pub fn under(&self, owner: impl Key) -> Counters {
        Counters { counters: self.counters.under(owner), pool: self.pool() }
    }

    /// The same counters, for their blocking calls.
    pub fn blocking(&self) -> &crate::kv::Counters {
        &self.counters
    }

    fn pool(&self) -> Arc<Pool> {
        Arc::clone(&self.pool)
    }

    fn runs(&self) -> Runs {
        if self.counters.on_a_copy() {
            Runs::Copy
        } else if self.counters.in_memory() {
            Runs::InMemory
        } else {
            Runs::Written
        }
    }
}

impl From<crate::kv::Counters> for Counters {
    /// The same counters, their calls awaited.
    fn from(counters: crate::kv::Counters) -> Counters {
        let pool = counters.scope.pool();
        Counters { counters, pool }
    }
}

impl Clone for Counters {
    fn clone(&self) -> Self {
        Counters { counters: self.counters.clone(), pool: self.pool() }
    }
}

impl fmt::Debug for Counters {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.counters, f)
    }
}

impl fmt::Debug for CountersBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.builder, f)
    }
}

impl Sealed for Counters {}

/// Counters whose calls are awaited join a transaction as the blocking ones
/// do; counters kept in memory join none, and their calls through it are
/// `Invalid`.
impl TxHandle for Counters {
    type InTx<'h> = TxCounters<'h>;

    fn in_tx<'h>(&'h self, tx: &'h Transaction<'h>) -> TxCounters<'h> {
        self.counters.in_tx(tx)
    }
}

#[cfg(test)]
#[path = "counters_tests.rs"]
mod tests;
