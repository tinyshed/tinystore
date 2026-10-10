use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::time::Duration;

use rusqlite::Connection;

use super::bucket::{Work, clear_work};
use super::buffer::{Buffer, Change, Live};
use super::cells;
use super::engine::next_version;
use super::path::Key;
use super::scope::{Kind, Scope};
use super::value::Raw;
use crate::clock::millis;
use crate::engine::Home;
use crate::sqlite::Tx;
use crate::{Error, Result, Store};

/// Counters being opened: their name, how long a counter lasts, and whether
/// they are kept in memory between flushes.
#[must_use = "counters open with open()"]
pub struct CountersBuilder {
    store: Store,
    name: String,
    ttl: Option<Duration>,
    flush_every: Option<Duration>,
}

impl Store {
    /// Numbers by key that add up, in the store's kv.db: attempts, hits, uses.
    pub fn counters(&self, name: &str) -> CountersBuilder {
        CountersBuilder { store: self.clone(), name: name.to_owned(), ttl: None, flush_every: None }
    }
}

impl CountersBuilder {
    /// A counter lasts this long from its first add, then starts again from 0:
    /// the adds after the first do not move it.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Keeps the counters in memory and writes what changed once a span, so
    /// that an add costs no commit and a crash loses at most the last span.
    /// Without it each add is written before it returns.
    pub fn flush_every(mut self, span: Duration) -> Self {
        self.flush_every = Some(span);
        self
    }

    pub fn open(self) -> Result<Counters> {
        let shown = || format!("kv counters {}", self.name);
        if self.ttl.is_some_and(|ttl| ttl.is_zero()) {
            return Err(Error::invalid("a ttl of zero").within(shown()));
        }
        if self.flush_every.is_some_and(|span| span.is_zero()) {
            return Err(Error::invalid("a flush every zero").within(shown()));
        }
        let scope = Scope::open(&Home::Store(self.store.clone()), &self.name, Kind::Counters)?;
        let buffer =
            scope.kv.buffer(scope.id, self.flush_every, &scope.shown()).map_err(|error| error.within(scope.shown()))?;
        Ok(Counters { scope, ttl: self.ttl, buffer })
    }
}

/// Numbers by key that only add up; a counter that is not there holds 0.
/// Every handle on a name counts the same way, in the file or in memory.
pub struct Counters {
    pub(crate) scope: Scope,
    ttl: Option<Duration>,
    pub(crate) buffer: Option<Arc<Buffer>>,
}

impl Counters {
    /// The same counters seen from the branch `owner` names under these.
    pub fn under(&self, owner: impl Key) -> Counters {
        Counters { scope: self.scope.under(owner), ttl: self.ttl, buffer: self.buffer.clone() }
    }

    /// Adds `n` to the counter under `key` and returns what it holds now. A
    /// counter that is not there starts from 0, its ttl from now; a sum past
    /// the range of an `i64` is `Limit` and changes nothing.
    pub fn add(&self, key: impl Key, n: i64) -> Result<i64> {
        let key = key.text();
        let Some(buffer) = &self.buffer else {
            return self.scope.write(&key, 0, self.add_work(&key, n)?);
        };
        let (path, now) = (self.scope.path(&key)?, self.scope.now());
        let created = self.created(now);
        let step = |live: Option<Live>| {
            let (held, expires) = live.map_or((0, created), |live| (live.value, live.expires));
            let sum = held.checked_add(n).ok_or_else(|| overflow(n))?;
            Ok((Change::Set(Live { value: sum, expires }), sum))
        };
        buffer.step(&self.scope.kv, &path, now, step).map_err(|error| error.within(self.scope.shown_key(&key)))
    }

    /// `add` without waiting for its commit: `done` gets the sum on the
    /// thread that commits it. Counters kept in memory answer before this
    /// returns.
    pub(crate) fn add_then(&self, key: &str, n: i64, done: impl FnOnce(Result<i64>) + Send + 'static) {
        if self.buffer.is_some() {
            return done(self.add(key, n));
        }
        match self.add_work(key, n) {
            Ok(work) => self.scope.submit(key, 0, work, done),
            Err(error) => done(Err(error)),
        }
    }

    /// `delete` without waiting for its commit.
    pub(crate) fn delete_then(&self, key: &str, done: impl FnOnce(Result<bool>) + Send + 'static) {
        if self.buffer.is_some() {
            return done(self.delete(key));
        }
        match self.delete_work(key) {
            Ok(work) => self.scope.submit(key, 0, work, done),
            Err(error) => done(Err(error)),
        }
    }

    /// What the counter under `key` holds; 0 when it is not there.
    pub fn get(&self, key: impl Key) -> Result<i64> {
        let key = key.text();
        let (id, path, now) = (self.scope.id, self.scope.path(&key)?, self.scope.now());
        let live = match &self.buffer {
            Some(buffer) => buffer.get(&self.scope.kv, &path, now),
            None => self.scope.kv.file().read(|connection| live(connection, id, &path, now)),
        };
        let live = live.map_err(|error| error.within(self.scope.shown_key(&key)))?;
        Ok(live.map_or(0, |live| live.value))
    }

    /// Removes the counter under `key` and says whether it was there.
    pub fn delete(&self, key: impl Key) -> Result<bool> {
        let key = key.text();
        let Some(buffer) = &self.buffer else {
            return self.scope.write(&key, 0, self.delete_work(&key)?);
        };
        let (path, now) = (self.scope.path(&key)?, self.scope.now());
        let step =
            |live: Option<Live>| Ok((if live.is_some() { Change::Remove } else { Change::Leave }, live.is_some()));
        buffer.step(&self.scope.kv, &path, now, step).map_err(|error| error.within(self.scope.shown_key(&key)))
    }

    /// Removes every counter of this branch and of the branches under it, at
    /// once for every reader, however many there are.
    pub fn clear(&self) -> Result<()> {
        let work = clear_work(&self.scope)?;
        let prefix = self.scope.branch()?.prefix().to_vec();
        let revision = self.scope.kv.revision();
        let file = self.scope.kv.file();
        let write = move || file.write(0, move |tx| work(tx, &revision));
        let cleared = match &self.buffer {
            Some(buffer) => buffer.clear(&prefix, write),
            None => write(),
        };
        cleared.map_err(|error| error.within(self.scope.shown_branch()))
    }

    pub(crate) fn add_work(&self, key: &str, n: i64) -> Result<impl Work<i64> + use<>> {
        let now = self.scope.now();
        let (id, path, created) = (self.scope.id, self.scope.path(key)?, self.created(now));
        Ok(move |tx: &Tx<'_>, revision: &AtomicI64| {
            let version = next_version(revision, tx)?;
            cells::add(tx, id, &path, (version, created), n, now)?.ok_or_else(|| overflow(n))
        })
    }

    pub(crate) fn delete_work(&self, key: &str) -> Result<impl Work<bool> + use<>> {
        let now = self.scope.now();
        let (id, path) = (self.scope.id, self.scope.path(key)?);
        Ok(move |tx: &Tx<'_>, _: &AtomicI64| {
            let was_live = cells::current(tx, id, &path, now)?.is_some_and(|current| current.live);
            cells::remove(tx, id, &path, None)?;
            Ok(was_live)
        })
    }

    /// The counter under `key` as `connection` sees it.
    pub(crate) fn get_on(&self, connection: &Connection, key: &str) -> Result<i64> {
        let (id, path, now) = (self.scope.id, self.scope.path(key)?, self.scope.now());
        Ok(live(connection, id, &path, now)?.map_or(0, |live| live.value))
    }

    fn created(&self, now: i64) -> Option<i64> {
        self.ttl.map(|ttl| now.saturating_add(millis(ttl)))
    }
}

impl Clone for Counters {
    fn clone(&self) -> Self {
        Counters { scope: self.scope.clone(), ttl: self.ttl, buffer: self.buffer.clone() }
    }
}

impl fmt::Debug for Counters {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Counters({:?})", self.scope)
    }
}

impl fmt::Debug for CountersBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CountersBuilder({})", self.name)
    }
}

/// A counter's row as a live counter, read on `connection`.
pub(crate) fn live(connection: &Connection, id: i64, path: &[u8], now: i64) -> Result<Option<Live>> {
    let Some(cell) = cells::live(connection, id, path, now)? else {
        return Ok(None);
    };
    match cell.raw {
        Raw::Int(value) => Ok(Some(Live { value, expires: cell.expires })),
        _ => Err(Error::corrupt("a counter's row holds no integer")),
    }
}

fn overflow(n: i64) -> Error {
    Error::limit(format!("adding {n} passes the range of an i64"))
}

#[cfg(test)]
#[path = "counters_tests.rs"]
mod tests;
