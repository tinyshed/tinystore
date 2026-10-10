//! A checked batch: a transaction made by a host that never holds the writer
//! while it awaits. Its reads ran before, each kept with what it found; the
//! batch checks each is still so and applies its writes, all in one grouped
//! write, or none of them.

use std::sync::Arc;
use std::sync::atomic::AtomicI64;

use super::bucket::{Bucket, Put, Stamp, WriteOptions, clear_work};
use super::cells::{self, Cell};
use super::counters::Counters;
use super::engine::Kv;
use super::scope::Scope;
use super::value::Raw;
use crate::sqlite::Tx;
use crate::{Error, ErrorKind, Result};

type Step = Box<dyn FnOnce(&Tx<'_>, &AtomicI64) -> Result<Outcome> + Send>;

/// Checks and writes to commit together.
#[derive(Default)]
pub(crate) struct Batch {
    kv: Option<Arc<Kv>>,
    checks: Vec<Step>,
    writes: Vec<Step>,
    bytes: usize,
}

/// What one write did.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Outcome {
    pub(crate) found: bool,
    pub(crate) written: bool,
    pub(crate) value: Option<Raw>,
    pub(crate) version: Option<i64>,
    pub(crate) expires: Option<i64>,
    pub(crate) count: Option<i64>,
}

/// A batch's outcomes, or its failure and the place of what failed.
pub(crate) type Committed = std::result::Result<Vec<Outcome>, (Option<Place>, Error)>;

/// Where a batch failed: the check or the write, by its place.
#[derive(Debug)]
pub(crate) enum Place {
    Check(usize),
    Write(usize),
}

impl Batch {
    /// Checks that `key` is still at `version`, or still absent when `None`.
    pub(crate) fn check(&mut self, bucket: &Bucket<()>, key: &str, version: Option<i64>) -> Result<()> {
        let (id, path, now) = self.place(&bucket.scope, key)?;
        let shown = bucket.scope.shown_key(key);
        self.checks.push(Box::new(move |tx, _| {
            let live =
                cells::current(tx, id, &path, now)?.filter(|current| current.live).map(|current| current.version);
            if live != version {
                return Err(Error::new(ErrorKind::Conflict, "changed since it was read").within(shown));
            }
            Ok(Outcome::default())
        }));
        Ok(())
    }

    pub(crate) fn put(
        &mut self,
        bucket: &Bucket<()>,
        key: &str,
        (raw, options, put): (Raw, WriteOptions, Put),
    ) -> Result<()> {
        self.join(&bucket.scope)?;
        let (bytes, work) = bucket.put_work(key, raw, options, put)?;
        self.bytes += bytes;
        self.write(&bucket.scope, key, move |tx, revision| work(tx, revision).map(stamped))
    }

    /// A take, which answers the value it removed.
    pub(crate) fn take(&mut self, bucket: &Bucket<()>, key: &str, options: WriteOptions) -> Result<()> {
        self.join(&bucket.scope)?;
        let work = bucket.take_cell_work(key, options)?;
        self.write(&bucket.scope, key, move |tx, revision| work(tx, revision).map(taken))
    }

    /// A delete, which answers whether a key was there and reads no value.
    pub(crate) fn remove(&mut self, bucket: &Bucket<()>, key: &str, options: WriteOptions) -> Result<()> {
        self.join(&bucket.scope)?;
        let work = bucket.remove_work(key, options)?;
        self.write(&bucket.scope, key, move |tx, revision| {
            work(tx, revision).map(|found| Outcome { found, ..Outcome::default() })
        })
    }

    pub(crate) fn expire(&mut self, bucket: &Bucket<()>, key: &str, options: WriteOptions) -> Result<()> {
        self.join(&bucket.scope)?;
        let work = bucket.expire_work(key, options)?;
        self.write(&bucket.scope, key, move |tx, revision| {
            work(tx, revision).map(|found| Outcome { found, ..Outcome::default() })
        })
    }

    pub(crate) fn clear(&mut self, scope: &Scope) -> Result<()> {
        self.join(scope)?;
        let work = clear_work(scope)?;
        let shown = scope.shown_branch();
        self.writes.push(Box::new(move |tx, revision| {
            work(tx, revision).map(|()| Outcome::default()).map_err(|error| error.within(shown))
        }));
        Ok(())
    }

    pub(crate) fn add(&mut self, counters: &Counters, key: &str, n: i64) -> Result<()> {
        if counters.buffer.is_some() {
            return Err(Error::invalid("counters kept in memory join no transaction").within(counters.scope.shown()));
        }
        self.join(&counters.scope)?;
        let work = counters.add_work(key, n)?;
        self.write(&counters.scope, key, move |tx, revision| {
            work(tx, revision).map(|count| Outcome { count: Some(count), ..Outcome::default() })
        })
    }

    /// Checks every read, then applies every write, in one grouped write; the
    /// first that fails rolls all of them back and says its place. It does
    /// not wait: `done` gets the outcomes on the thread that commits them.
    pub(crate) fn commit_then(self, done: impl FnOnce(Committed) + Send + 'static) {
        let Some(kv) = self.kv else {
            return done(Ok(Vec::new()));
        };
        let (checks, writes, revision) = (self.checks, self.writes, kv.revision());
        let failed = Arc::new(std::sync::Mutex::new(None));
        let at = Arc::clone(&failed);
        let work = move |tx: &Tx<'_>| {
            for (index, check) in checks.into_iter().enumerate() {
                check(tx, &revision).inspect_err(|_| *lock(&at) = Some(Place::Check(index)))?;
            }
            let mut outcomes = Vec::with_capacity(writes.len());
            for (index, write) in writes.into_iter().enumerate() {
                outcomes.push(write(tx, &revision).inspect_err(|_| *lock(&at) = Some(Place::Write(index)))?);
            }
            Ok(outcomes)
        };
        let answered = move |committed: Result<Vec<Outcome>>| {
            done(committed.map_err(|error| (lock(&failed).take(), error)));
        };
        kv.file().submit(self.bytes, work, answered);
    }

    fn write(
        &mut self,
        scope: &Scope,
        key: &str,
        work: impl FnOnce(&Tx<'_>, &AtomicI64) -> Result<Outcome> + Send + 'static,
    ) -> Result<()> {
        let shown = scope.shown_key(key);
        self.writes.push(Box::new(move |tx, revision| work(tx, revision).map_err(|error| error.within(shown))));
        Ok(())
    }

    fn place(&mut self, scope: &Scope, key: &str) -> Result<(i64, Vec<u8>, i64)> {
        self.join(scope)?;
        Ok((scope.id, scope.path(key)?, scope.now()))
    }

    /// Takes the batch's file from its first handle, and refuses a handle of
    /// another store.
    fn join(&mut self, scope: &Scope) -> Result<()> {
        match &self.kv {
            None => {
                self.kv = Some(Arc::clone(&scope.kv));
                Ok(())
            }
            Some(kv) if Arc::ptr_eq(kv, &scope.kv) => Ok(()),
            Some(_) => Err(Error::invalid("a handle of another store, in a batch of this one").within(scope.shown())),
        }
    }
}

impl std::fmt::Debug for Batch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Batch({} checks, {} writes)", self.checks.len(), self.writes.len())
    }
}

fn stamped(stamp: Stamp) -> Outcome {
    Outcome {
        written: stamp.written,
        version: Some(stamp.version.revision()),
        expires: stamp.expires,
        ..Outcome::default()
    }
}

fn taken(cell: Option<Cell>) -> Outcome {
    match cell {
        Some(cell) => Outcome { found: true, value: Some(cell.raw), ..Outcome::default() },
        None => Outcome::default(),
    }
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
