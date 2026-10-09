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
use crate::sqlite::Tx;
use crate::{Durability, Error, Result, Store};

/// Counters being opened: their name, how long a counter lasts, how far an
/// add goes before it returns.
#[must_use = "counters open with open()"]
pub struct CountersBuilder {
    store: Store,
    name: String,
    ttl: Option<Duration>,
    durability: Durability,
}

impl Store {
    /// Numbers by key that add up, in the store's kv.db: attempts, hits, uses.
    pub fn counters(&self, name: &str) -> CountersBuilder {
        CountersBuilder { store: self.clone(), name: name.to_owned(), ttl: None, durability: Durability::Full }
    }
}

impl CountersBuilder {
    /// A counter lasts this long from its first add, then starts again from 0:
    /// the adds after the first do not move it.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// `Full`, the default, writes each add before it returns. An interval
    /// keeps the counters in memory and writes what changed once an interval,
    /// so an add costs no commit and a crash loses at most the last interval.
    pub fn durability(mut self, durability: impl Into<Durability>) -> Self {
        self.durability = durability.into();
        self
    }

    pub fn open(self) -> Result<Counters> {
        let shown = || format!("kv counters {}", self.name);
        let every = interval(self.durability).map_err(|error| error.within(shown()))?;
        if self.ttl.is_some_and(|ttl| ttl.is_zero()) {
            return Err(Error::invalid("a ttl of zero").within(shown()));
        }
        let scope = Scope::open(&self.store, &self.name, Kind::Counters)?;
        let buffer = scope.kv.buffer(scope.id, every, &scope.shown()).map_err(|error| error.within(scope.shown()))?;
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

/// How often counters kept in memory are written, or none when each add is.
fn interval(durability: Durability) -> Result<Option<Duration>> {
    match durability {
        Durability::Full => Ok(None),
        Durability::Every(every) if every.is_zero() => Err(Error::invalid("a durability interval of zero")),
        Durability::Every(every) => Ok(Some(every)),
        Durability::Os => Err(Error::invalid(
            "durability Os is kv.db's to choose, not a counter's: an add is written in full, or kept in memory and \
             written every interval",
        )),
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
