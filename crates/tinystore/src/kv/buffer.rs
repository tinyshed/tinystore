//! Counters kept in memory between flushes: counters opened with
//! `flush_every`, and the times of a rate limit.
//!
//! For the keys it holds a buffer is the truth, and kv.db is behind it by what
//! changed since the last flush; a key it does not hold is as the file has it.
//! A flush writes what changed, then lets go of what the file now holds as
//! memory does, so that memory holds what is busy and not every key ever seen.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use super::cells;
use super::engine::{Kv, next_version};
use super::path::is_under;
use super::value::Raw;
use crate::{Error, ErrorKind, Result};

/// Counters a buffer holds, changed or not: about a second of flushing. A
/// change of a key past them flushes first.
const MOST_HELD: usize = 100_000;

/// Counters one commit of a flush writes.
const BATCH: usize = 10_000;

pub(crate) struct Buffer {
    bucket: i64,
    every: Duration,
    held: Mutex<Held>,
    /// One flush at a time, and none while a clear runs.
    flushing: Mutex<()>,
}

struct Held {
    counters: HashMap<Vec<u8>, Counter>,
    /// Moves on whenever counters leave memory: a change that read the file
    /// before they left reads it again rather than keep what it read.
    evictions: u64,
    most: usize,
}

#[derive(Clone, Copy, Debug)]
struct Counter {
    value: i64,
    expires: Option<i64>,
    /// A delete said so, or the file held none.
    absent: bool,
    /// Changed since a flush took it.
    dirty: bool,
}

/// A counter that is there and has not expired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Live {
    pub(crate) value: i64,
    pub(crate) expires: Option<i64>,
}

/// What a step does to the counter it found.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Change {
    Leave,
    Set(Live),
    Remove,
}

type Taken = Vec<(Vec<u8>, Counter)>;

impl Buffer {
    pub(crate) fn new(bucket: i64, every: Duration) -> Buffer {
        let held = Held { counters: HashMap::new(), evictions: 0, most: MOST_HELD };
        Buffer { bucket, every, held: Mutex::new(held), flushing: Mutex::new(()) }
    }

    #[cfg(test)]
    pub(crate) fn bound(&self, most: usize) {
        self.lock().most = most;
    }

    #[cfg(test)]
    pub(crate) fn held(&self) -> usize {
        self.lock().counters.len()
    }

    pub(crate) fn every(&self) -> Duration {
        self.every
    }

    /// Applies `change` to the counter at `path`, given what it holds at `now`,
    /// and returns the answer `change` gives. A key memory does not hold is
    /// read from the file first; one past the bound waits for a flush, and a
    /// flush that fails refuses the change.
    pub(crate) fn step<T>(
        &self,
        kv: &Kv,
        path: &[u8],
        now: i64,
        change: impl FnOnce(Option<Live>) -> Result<(Change, T)>,
    ) -> Result<T> {
        loop {
            let evictions = {
                let mut held = self.lock();
                if let Some(counter) = held.counters.get_mut(path) {
                    return counter.apply(now, change);
                }
                held.evictions
            };
            let loaded = self.load(kv, path, now)?;
            let mut held = self.lock();
            if held.evictions != evictions {
                continue;
            }
            if !held.counters.contains_key(path) && held.counters.len() >= held.most {
                drop(held);
                self.flush(kv)?;
                continue;
            }
            return held.counters.entry(path.to_vec()).or_insert(loaded).apply(now, change);
        }
    }

    /// The counter at `path` at `now`, from memory or else the file.
    pub(crate) fn get(&self, kv: &Kv, path: &[u8], now: i64) -> Result<Option<Live>> {
        if let Some(counter) = self.lock().counters.get(path) {
            return Ok(counter.live(now));
        }
        Ok(self.load(kv, path, now)?.live(now))
    }

    /// Writes every counter changed since the last flush, a commit a batch, and
    /// says how many. A failed commit leaves its counters changed for the next.
    pub(crate) fn flush(&self, kv: &Kv) -> Result<usize> {
        let _flushing = lock(&self.flushing);
        let mut written = 0;
        loop {
            let batch = self.take_changed();
            let count = batch.len();
            if count == 0 {
                break;
            }
            let paths: Vec<Vec<u8>> = batch.iter().map(|(path, _)| path.clone()).collect();
            if let Err(error) = self.write(kv, batch) {
                self.give_back(&paths);
                return Err(error);
            }
            written += count;
            if count < BATCH {
                break;
            }
        }
        self.evict();
        Ok(written)
    }

    /// Runs a clear of the branch under `prefix` with no flush beside it, and
    /// lets go of what memory holds under the branch once it commits, so that no
    /// flush writes a cleared counter again. A clear that failed leaves memory
    /// as it was; one whose commit failed may be in the file, so memory lets go
    /// as a crash would.
    pub(crate) fn clear(&self, prefix: &[u8], clear: impl FnOnce() -> Result<()>) -> Result<()> {
        let _flushing = lock(&self.flushing);
        let cleared = clear();
        if let Err(error) = &cleared
            && error.kind() != ErrorKind::OutcomeUnknown
        {
            return cleared;
        }
        let mut held = self.lock();
        held.counters.retain(|path, _| !is_under(path, prefix));
        held.evictions += 1;
        cleared
    }

    fn load(&self, kv: &Kv, path: &[u8], now: i64) -> Result<Counter> {
        let bucket = self.bucket;
        let found = kv.file().read(|connection| cells::live(connection, bucket, path, now))?;
        let Some(cell) = found else {
            return Ok(Counter { value: 0, expires: None, absent: true, dirty: false });
        };
        let Raw::Int(value) = cell.raw else {
            return Err(Error::corrupt("a counter's row holds no integer"));
        };
        Ok(Counter { value, expires: cell.expires, absent: false, dirty: false })
    }

    /// Takes a batch of changed counters, marking them written: a change after
    /// it marks one changed again.
    fn take_changed(&self) -> Taken {
        let mut held = self.lock();
        let mut batch = Vec::new();
        for (path, counter) in held.counters.iter_mut().filter(|(_, counter)| counter.dirty).take(BATCH) {
            counter.dirty = false;
            batch.push((path.clone(), *counter));
        }
        batch
    }

    fn write(&self, kv: &Kv, batch: Taken) -> Result<()> {
        let (bucket, revision) = (self.bucket, kv.revision());
        kv.file().write(0, move |tx| {
            let version = next_version(&revision, tx)?;
            for (path, counter) in &batch {
                if counter.absent {
                    cells::remove(tx, bucket, path, None)?;
                } else {
                    cells::put(tx, bucket, path, (version, counter.expires), &Raw::Int(counter.value), None)?;
                }
            }
            Ok(())
        })
    }

    /// Marks changed again what a failed flush took; the flush lock keeps them
    /// in memory meanwhile.
    fn give_back(&self, paths: &[Vec<u8>]) {
        let mut held = self.lock();
        for path in paths {
            if let Some(counter) = held.counters.get_mut(path) {
                counter.dirty = true;
            }
        }
    }

    /// Lets go of the counters the file holds as memory does.
    fn evict(&self) {
        let mut held = self.lock();
        let before = held.counters.len();
        held.counters.retain(|_, counter| counter.dirty);
        if held.counters.len() != before {
            held.evictions += 1;
        }
    }

    fn lock(&self) -> MutexGuard<'_, Held> {
        lock(&self.held)
    }
}

impl Counter {
    fn live(&self, now: i64) -> Option<Live> {
        let expired = self.expires.is_some_and(|expires| expires <= now);
        (!self.absent && !expired).then_some(Live { value: self.value, expires: self.expires })
    }

    fn apply<T>(&mut self, now: i64, change: impl FnOnce(Option<Live>) -> Result<(Change, T)>) -> Result<T> {
        let (change, answer) = change(self.live(now))?;
        match change {
            Change::Leave => {}
            Change::Set(live) => {
                *self = Counter { value: live.value, expires: live.expires, absent: false, dirty: true }
            }
            Change::Remove => *self = Counter { value: 0, expires: None, absent: true, dirty: true },
        }
        Ok(answer)
    }
}

impl std::fmt::Debug for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Buffer({} counters, every {:?})", self.lock().counters.len(), self.every)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
