use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use super::buffer::Buffer;
use super::cells;
use super::once::Runs;
use super::renewals::Renewals;
use super::scope::Kind;
use crate::engine::{Claim, Engine, Home, Host};
use crate::sqlite::{Config, File, Migration, Tx};
use crate::store::WeakStore;
use crate::{Clock, Error, Result, Store, unix_millis};

/// kv.db's schema, a step a file; until `v0.1.0` the first step is edited
/// rather than a second added.
const MIGRATIONS: &[Migration] = &[Migration::of(1, "0001_schema.sql", include_str!("migrations/0001_schema.sql"))];

/// Expired cells a maintenance transaction deletes, and transactions a call.
const EXPIRE_BATCH: usize = 10_000;
const BATCHES: usize = 10;

/// How often the store runs kv's maintenance, and writes the renewals reads
/// of idle keys asked for.
const MAINTENANCE: Duration = Duration::from_secs(60);
const RENEWALS: Duration = Duration::from_secs(1);

/// kv in one file: kv.db, its one writer, and the revision every version
/// comes from, which every handle of the store shares; or a database's file,
/// which every handle opened from the database shares.
pub(crate) struct Kv {
    file: Arc<File>,
    /// The engine whose file kv is kept in, when it is not kv.db: `sql app`.
    lent_by: Option<String>,
    clock: Arc<dyn Clock>,
    /// Weak, so that the application's last handle on the store closes it.
    store: WeakStore,
    revision: Arc<AtomicI64>,
    names: Mutex<HashMap<String, (i64, Kind)>>,
    /// How each name of counters counts in this process, every handle on it
    /// alike: in the file, or in one buffer.
    counting: Mutex<HashMap<i64, Option<Arc<Buffer>>>>,
    renewals: Renewals,
    runs: Runs,
    _claim: Option<Claim>,
}

/// What one maintenance pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Maintenance {
    /// Cells deleted because they expired.
    pub expired: usize,
    /// Cells deleted because a large clear hid them.
    pub cleared: usize,
    /// Idle keys whose renewal was written.
    pub renewed: usize,
    /// Counters written from memory.
    pub flushed: usize,
}

impl Kv {
    /// The store's kv engine, in kv.db, opened by the first handle.
    pub(crate) fn of(store: &Store) -> Result<Arc<Kv>> {
        store.engine(|store| {
            let claim = store.claim("kv.db")?;
            let file = File::open(claim.path(), Config::default())?;
            Kv::start(store, Arc::new(file), None, Some(claim))
        })
    }

    /// The kv engine a handle opens in: the store's, or the one kept in the
    /// file of the database it was opened from.
    pub(crate) fn at(home: &Home) -> Result<Arc<Kv>> {
        match home {
            Home::Store(store) => Kv::of(store),
            Home::Shared(shared) => shared.engine(|store| {
                let lent_by = Some(shared.owner().to_owned());
                Kv::start(store, Arc::clone(shared.file()), lent_by, None)
            }),
        }
    }

    /// Makes kv's tables in `file` when it has none, and starts its
    /// maintenance and its renewals on the store's background thread.
    fn start(store: &Store, file: Arc<File>, lent_by: Option<String>, claim: Option<Claim>) -> Result<Arc<Kv>> {
        file.migrate("kv", MIGRATIONS)?;
        let revision = file.read(cells::revision)?;
        let tasks = lent_by.as_ref().map_or_else(|| "kv".to_owned(), |owner| format!("kv in {owner}"));
        let kv = Arc::new(Kv {
            file,
            lent_by,
            clock: store.clock(),
            store: store.downgrade(),
            revision: Arc::new(AtomicI64::new(revision)),
            names: Mutex::new(HashMap::new()),
            counting: Mutex::new(HashMap::new()),
            renewals: Renewals::default(),
            runs: Runs::default(),
            _claim: claim,
        });
        let weak = Arc::downgrade(&kv);
        store
            .every(&format!("{tasks}: maintenance"), MAINTENANCE, move || with(&weak, |kv| kv.maintain().map(drop)))?;
        let weak = Arc::downgrade(&kv);
        let renew = move || with(&weak, |kv| kv.renewals.flush(&kv.file).map(drop));
        store.every(&format!("{tasks}: renewals"), RENEWALS, renew)?;
        Ok(kv)
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    /// The engine whose file kv is kept in, when it is not kv.db.
    pub(crate) fn lent_by(&self) -> Option<&str> {
        self.lent_by.as_deref()
    }

    /// The file as an error names it: `kv.db`, or `sql app`.
    pub(crate) fn place(&self) -> &str {
        self.lent_by.as_deref().unwrap_or("kv.db")
    }

    pub(crate) fn now(&self) -> i64 {
        unix_millis(self.clock.now())
    }

    pub(crate) fn clock(&self) -> &dyn Clock {
        &*self.clock
    }

    pub(crate) fn revision(&self) -> Arc<AtomicI64> {
        Arc::clone(&self.revision)
    }

    pub(crate) fn renewals(&self) -> &Renewals {
        &self.renewals
    }

    pub(crate) fn runs(&self) -> &Runs {
        &self.runs
    }

    /// The id of `name`, made the first time it opens; a name opened as
    /// another kind is refused.
    pub(crate) fn name(&self, name: &str, kind: Kind) -> Result<i64> {
        let known = lock(&self.names).get(name).copied();
        let (id, role) = match known {
            Some((id, held)) => (id, held.role().to_owned()),
            None => self.file.transaction(|tx| cells::bucket(tx, name, kind.role()))?,
        };
        if role != kind.role() {
            return Err(Error::invalid(format!("the name holds {}, not {}", described(&role), described(kind.role()))));
        }
        lock(&self.names).insert(name.to_owned(), (id, kind));
        Ok(id)
    }

    /// The buffer that the counters of `id` keep in this process, or none when
    /// each change is written; every handle on a name counts one way, and the
    /// buffer flushes on its own every interval.
    pub(crate) fn buffer(
        self: &Arc<Self>,
        id: i64,
        every: Option<Duration>,
        shown: &str,
    ) -> Result<Option<Arc<Buffer>>> {
        let mut counting = lock(&self.counting);
        if let Some(found) = counting.get(&id) {
            let found_every = found.as_ref().map(|buffer| buffer.every());
            if found_every != every {
                let (found, asked) = (kept(found_every), kept(every));
                return Err(Error::invalid(format!("open in this process {found}, not {asked}")));
            }
            return Ok(found.clone());
        }
        let buffer = every.map(|every| Arc::new(Buffer::new(id, every)));
        counting.insert(id, buffer.clone());
        drop(counting);
        if let (Some(buffer), Some(store)) = (&buffer, self.store.upgrade()) {
            let (kv, flushed) = (Arc::downgrade(self), Arc::downgrade(buffer));
            store.every(&format!("{}: {shown}", self.place()), buffer.every(), move || flush(&kv, &flushed))?;
        }
        Ok(buffer)
    }

    /// Writes what memory holds, then deletes the cells that expired and those
    /// a large clear hid, a bounded number of transactions at a time. No read
    /// sees either kind before it.
    pub(crate) fn maintain(&self) -> Result<Maintenance> {
        let mut done = Maintenance {
            renewed: self.renewals.flush(&self.file)?,
            flushed: self.flush_buffers()?,
            ..Maintenance::default()
        };
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

    /// Flushes every buffer; the first that fails is the error, the others
    /// still flush.
    fn flush_buffers(&self) -> Result<usize> {
        let buffers: Vec<Arc<Buffer>> = lock(&self.counting).values().flatten().cloned().collect();
        let mut flushed = 0;
        let mut first_failure = None;
        for buffer in buffers {
            match buffer.flush(self) {
                Ok(written) => flushed += written,
                Err(error) => {
                    first_failure.get_or_insert(error);
                }
            }
        }
        first_failure.map_or(Ok(flushed), Err)
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

fn with(kv: &Weak<Kv>, work: impl FnOnce(&Kv) -> Result<()>) -> Result<()> {
    kv.upgrade().map_or(Ok(()), |kv| work(&kv))
}

fn flush(kv: &Weak<Kv>, buffer: &Weak<Buffer>) -> Result<()> {
    match (kv.upgrade(), buffer.upgrade()) {
        (Some(kv), Some(buffer)) => buffer.flush(&kv).map(drop),
        _ => Ok(()),
    }
}

/// A kind as an error says it: `the name holds a quota, not values`.
fn described(role: &str) -> &str {
    match role {
        "rate limit" => "a rate limit",
        "quota" => "a quota",
        "once" => "once's answers",
        other => other,
    }
}

/// How counters are kept, as an error says it.
fn kept(every: Option<Duration>) -> String {
    every.map_or_else(|| "writing each add".to_owned(), |every| format!("flushing every {every:?}"))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Engine for Kv {
    /// Writes what memory holds, then closes kv.db; the first failure is the
    /// error, and the file closes either way. A file another engine lends is
    /// that engine's to close.
    fn close(&self) -> Result<()> {
        let renewed = self.renewals.flush(&self.file);
        let flushed = self.flush_buffers();
        let closed = if self.lent_by.is_none() { self.file.close() } else { Ok(()) };
        renewed.and(flushed).and(closed)
    }
}

impl std::fmt::Debug for Kv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Kv({})", self.file.path().display())
    }
}
