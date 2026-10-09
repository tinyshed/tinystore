use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use super::cells;
use crate::engine::{Claim, Engine, Host};
use crate::sqlite::{Config, File, Migration, Tx};
use crate::{Result, Store, unix_millis};

/// kv.db's schema, a step a file; until `v0.1.0` the first step is edited
/// rather than a second added.
const MIGRATIONS: &[Migration] = &[Migration { version: 1, sql: include_str!("migrations/0001_schema.sql") }];

/// Expired cells a maintenance transaction deletes, and transactions a call.
const EXPIRE_BATCH: usize = 10_000;
const BATCHES: usize = 10;

/// How often the store runs kv's maintenance.
const MAINTENANCE: Duration = Duration::from_secs(60);

/// The kv engine of a store: kv.db, its one writer, and the revision every
/// version comes from. Every bucket of the store shares it.
pub(crate) struct Kv {
    file: File,
    store: Store,
    revision: Arc<AtomicI64>,
    buckets: Mutex<HashMap<(String, &'static str), i64>>,
    _claim: Claim,
}

/// What one maintenance pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Maintenance {
    /// Cells deleted because they expired.
    pub expired: usize,
    /// Cells deleted because a large clear hid them.
    pub cleared: usize,
}

impl Kv {
    /// The store's kv engine, opened by the first bucket.
    pub(crate) fn of(store: &Store) -> Result<Arc<Kv>> {
        store.engine(|store| {
            let kv = Arc::new(Kv::open(store)?);
            let weak = Arc::downgrade(&kv);
            store.every("kv: maintenance", MAINTENANCE, move || maintain(&weak))?;
            Ok(kv)
        })
    }

    fn open(store: &Store) -> Result<Kv> {
        let claim = store.claim("kv.db")?;
        let file = File::open(claim.path(), Config::default())?;
        file.migrate("kv", MIGRATIONS)?;
        let revision = file.read(cells::revision)?;
        Ok(Kv {
            file,
            store: store.clone(),
            revision: Arc::new(AtomicI64::new(revision)),
            buckets: Mutex::new(HashMap::new()),
            _claim: claim,
        })
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    pub(crate) fn now(&self) -> i64 {
        unix_millis(self.store.now())
    }

    pub(crate) fn revision(&self) -> Arc<AtomicI64> {
        Arc::clone(&self.revision)
    }

    /// The id of the bucket `name`, made the first time it opens.
    pub(crate) fn bucket(&self, name: &str, role: &'static str) -> Result<i64> {
        let key = (name.to_owned(), role);
        if let Some(&id) = self.buckets.lock().unwrap_or_else(PoisonError::into_inner).get(&key) {
            return Ok(id);
        }
        let id = self.file.transaction(|tx| cells::bucket(tx, name, role))?;
        self.buckets.lock().unwrap_or_else(PoisonError::into_inner).insert(key, id);
        Ok(id)
    }

    /// Deletes the cells that expired and those a large clear hid, a bounded
    /// number of transactions at a time. No read sees either kind before it.
    pub(crate) fn maintain(&self) -> Result<Maintenance> {
        let mut done = Maintenance::default();
        let now = self.now();
        for _ in 0..BATCHES {
            let expired = self.file.write(0, move |tx| cells::expire(tx, now, EXPIRE_BATCH))?;
            done.expired += expired;
            if expired < EXPIRE_BATCH {
                break;
            }
        }
        for mark in self.file.read(cells::marks)? {
            done.cleared += self.drop_hidden(mark)?;
        }
        self.file.sweep();
        Ok(done)
    }

    fn drop_hidden(&self, mark: cells::Mark) -> Result<usize> {
        let mut past = mark.prefix.clone();
        past.push(0x03);
        let mut dropped = 0;
        for _ in 0..BATCHES {
            let (mark, past) = (mark.clone(), past.clone());
            let batch = self.file.write(0, move |tx| cells::drop_hidden(tx, &mark, &past, EXPIRE_BATCH))?;
            dropped += batch;
            if batch < EXPIRE_BATCH {
                break;
            }
        }
        Ok(dropped)
    }
}

/// Takes the next version inside a write and keeps the revision's high-water
/// mark with it, so a version never repeats across a delete or a reopen. A
/// write rolled back leaves its number unused, which is all the rule needs.
pub(crate) fn next_version(revision: &AtomicI64, tx: &Tx<'_>) -> Result<i64> {
    let version = revision.fetch_add(1, Ordering::SeqCst) + 1;
    cells::keep_revision(tx, version)?;
    Ok(version)
}

fn maintain(kv: &Weak<Kv>) -> Result<()> {
    match kv.upgrade() {
        Some(kv) => kv.maintain().map(|_| ()),
        None => Ok(()),
    }
}

impl Engine for Kv {
    fn close(&self) -> Result<()> {
        self.file.close()
    }
}

impl std::fmt::Debug for Kv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Kv({})", self.file.path().display())
    }
}
