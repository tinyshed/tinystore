use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use super::buffer::Buffer;
use super::cells;
use super::once::Runs;
use super::renewals::Renewals;
use super::scope::Kind;
use crate::encryption::Keyring;
use crate::engine::{Claim, Copying, Engine, Home, Host, to_copy};
use crate::sqlite::{Config, File, Left, Migration, Settle, Tx, own_work};
use crate::store::WeakStore;
use crate::sync::lock;
use crate::{Clock, Error, Result, Store, unix_millis};

/// kv.db's schema, a step a file; until `v0.1.0` the first step is edited
/// rather than a second added.
const MIGRATIONS: &[Migration] = &[Migration::of(1, "0001_schema.sql", include_str!("migrations/0001_schema.sql"))];

/// kv's own file in the store's directory.
pub(crate) const FILE: &str = "kv.db";

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
    /// What kv tells the store's own metrics.
    probe: Arc<crate::probe::Probe>,
    revision: Arc<Revision>,
    /// The revision the last transaction to keep one kept in the file.
    kept: AtomicI64,
    names: Mutex<HashMap<String, (i64, Kind)>>,
    /// How each name of counters counts in this process, every handle on it
    /// alike: in the file, or in one buffer.
    counting: Mutex<HashMap<i64, Option<Arc<Buffer>>>>,
    renewals: Renewals,
    runs: Runs,
    /// Once's runs this home handed to other nodes, by token, and when.
    #[cfg(feature = "replication")]
    passed: Mutex<Passed>,
    /// The configs open in this process, a hub each, which every handle of
    /// one name shares.
    #[cfg(feature = "config")]
    configs: Mutex<HashMap<String, Weak<super::config::Hub>>>,
    _claim: Option<Claim>,
}

/// Once's runs a home handed to other nodes: the last token given, and each
/// run held by its token with when it was handed.
#[cfg(feature = "replication")]
type Passed = (u64, HashMap<u64, (super::once::Handed, std::time::Instant)>);

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
            let claim = store.claim(FILE)?;
            let file = Arc::new(store.open_file(claim.name(), Config::committing(store.durability()))?);
            store.keep_file(&file);
            Kv::start(store, file, None, Some(claim))
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
        crate::engine::migrate_in(store, &file, "kv", MIGRATIONS, lent_by.as_deref())?;
        let revision = file.read(cells::revision)?;
        let behind = file.is_copy();
        let tasks = lent_by.as_ref().map_or_else(|| "kv".to_owned(), |owner| format!("kv in {owner}"));
        let kv = Arc::new(Kv {
            file,
            lent_by,
            clock: store.clock(),
            store: store.downgrade(),
            probe: Arc::clone(store.probe()),
            revision: Arc::new(Revision { taken: AtomicI64::new(revision), behind: AtomicBool::new(behind) }),
            kept: AtomicI64::new(revision),
            names: Mutex::new(HashMap::new()),
            counting: Mutex::new(HashMap::new()),
            renewals: Renewals::default(),
            runs: Runs::default(),
            #[cfg(feature = "replication")]
            passed: Mutex::default(),
            #[cfg(feature = "config")]
            configs: Mutex::new(HashMap::new()),
            _claim: claim,
        });
        kv.file.settle_with(Arc::downgrade(&kv) as Weak<dyn Settle>);
        // A copy's expiry and renewals are its home's, and reach it as pages:
        // each turn asks, since a file's home may move while it is open.
        let weak = Arc::downgrade(&kv);
        let maintain = move || with(&weak, |kv| if kv.file.is_copy() { Ok(()) } else { kv.maintain().map(drop) });
        store.every(&format!("{tasks}: maintenance"), MAINTENANCE, maintain)?;
        let weak = Arc::downgrade(&kv);
        let renew = move || with(&weak, |kv| kv.flush_renewals().map(drop));
        store.every(&format!("{tasks}: renewals"), RENEWALS, renew)?;
        Ok(kv)
    }

    /// Keeps a run of once handed to another node, and gives its token; runs
    /// handed longer ago than a node may take are let go, their keys free.
    #[cfg(feature = "replication")]
    pub(crate) fn keep_passed(&self, handed: super::once::Handed) -> u64 {
        const TAKES_AT_MOST: std::time::Duration = std::time::Duration::from_secs(600);
        let mut passed = lock(&self.passed);
        passed.1.retain(|_, (_, since)| since.elapsed() < TAKES_AT_MOST);
        passed.0 += 1;
        let token = passed.0;
        passed.1.insert(token, (handed, std::time::Instant::now()));
        token
    }

    /// The run handed over by `token`, if it is still held.
    #[cfg(feature = "replication")]
    pub(crate) fn take_passed(&self, token: u64) -> Option<super::once::Handed> {
        lock(&self.passed).1.remove(&token).map(|(handed, _)| handed)
    }

    /// The store kv opened in, while it is open.
    #[cfg(any(feature = "replication", feature = "async"))]
    pub(crate) fn store(&self) -> Result<Store> {
        self.store.upgrade().ok_or_else(|| Error::closed("the store"))
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    pub(crate) fn lent_by(&self) -> Option<&str> {
        self.lent_by.as_deref()
    }

    /// The file as an error names it: `kv.db`, or `sql app`.
    pub(crate) fn place(&self) -> &str {
        self.lent_by.as_deref().unwrap_or(FILE)
    }

    pub(crate) fn now(&self) -> i64 {
        unix_millis(self.clock.now())
    }

    pub(crate) fn clock(&self) -> &dyn Clock {
        &*self.clock
    }

    pub(crate) fn revision(&self) -> Arc<Revision> {
        Arc::clone(&self.revision)
    }

    /// The store's encryption keys, for a bucket whose values are sealed
    /// with the keys of the ids `kept`, or with none yet. A bucket that has a
    /// key makes the store make none: the store makes a key only for a bucket
    /// that holds nothing sealed, while no bucket of its directory does, open
    /// or not, so that a key lost is never replaced unseen.
    pub(crate) fn encryption_key(&self, kept: Option<&str>) -> Result<Arc<Keyring>> {
        let store = self.store.upgrade().ok_or_else(|| Error::closed(format!("{}: its store", self.place())))?;
        match kept {
            None => store.encryption_key(super::encrypted::sealed_in),
            Some(kept) => store.encryption_key_there().map_err(|error| {
                let kept = super::encrypted::named(kept);
                error.within(format!("its values are encrypted with {kept}, and the store makes no other"))
            }),
        }
    }

    pub(crate) fn renewals(&self) -> &Renewals {
        &self.renewals
    }

    /// Writes the renewals reads asked for, and says how many keys they
    /// renewed; a copy passes them to its home, and says how many it passed.
    fn flush_renewals(&self) -> Result<usize> {
        match self.file.is_copy() {
            false => self.renewals.flush(&self.file),
            #[cfg(feature = "replication")]
            true => self.renewals.pass_home(&self.store()?),
            #[cfg(not(feature = "replication"))]
            true => Ok(0),
        }
    }

    pub(crate) fn runs(&self) -> &Runs {
        &self.runs
    }

    #[cfg(feature = "config")]
    pub(crate) fn configs(&self) -> &Mutex<HashMap<String, Weak<super::config::Hub>>> {
        &self.configs
    }

    /// The id of `name`, made the first time it opens; a name opened as
    /// another kind is refused.
    pub(crate) fn name(&self, name: &str, kind: Kind) -> Result<i64> {
        let known = lock(&self.names).get(name).copied();
        let (id, role) = match known {
            Some((id, held)) => (id, held.role().to_owned()),
            #[cfg(feature = "replication")]
            None if self.file.is_copy() => self.name_from_home(name, kind)?,
            // a name made before is read, so that opening it takes no writer
            None => match self.file.read(|connection| cells::find_bucket(connection, name))? {
                Some(found) => found,
                None => own_work(|| self.file.transaction(|tx| cells::bucket(tx, name, kind.role())))?,
            },
        };
        if role != kind.role() {
            return Err(Error::invalid(format!("the name holds {}, not {}", described(&role), described(kind.role()))));
        }
        lock(&self.names).insert(name.to_owned(), (id, kind));
        Ok(id)
    }

    /// The id of `name` in a copy of kv's file: the copy's, or the one its
    /// home makes when the name is new.
    #[cfg(feature = "replication")]
    fn name_from_home(&self, name: &str, kind: Kind) -> Result<(i64, String)> {
        if let Some(found) = self.file.read(|connection| cells::find_bucket(connection, name))? {
            return Ok(found);
        }
        let database = self.lent_by.as_deref().and_then(crate::replication::database_of).map(str::to_owned);
        let handle = super::forward::Handle::Name { name: name.to_owned(), role: kind.role().to_owned(), database };
        super::forward::pass_on(&self.store()?, &handle, &[], "", &super::forward::Call::Open)?;
        self.file.read(|connection| cells::find_bucket(connection, name))?.ok_or_else(|| {
            Error::new(crate::ErrorKind::Unavailable, "its home made the name, and this copy does not hold it yet")
        })
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
        // kv.db, or a database's file that lends kv, `sql app`, as `sql/app`
        let file = crate::probe::label(&self.place().replacen(' ', "/", 1));
        self.probe.count("tinystore_kv_expired_total", &[("file", &file)], done.expired as f64);
        self.probe.count("tinystore_kv_cleared_total", &[("file", &file)], done.cleared as f64);
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

/// Where every version of a file comes from: the last one taken here, and
/// whether the file may hold a later one.
#[derive(Debug)]
pub(crate) struct Revision {
    taken: AtomicI64,
    /// Set while the file is a copy, whose home takes versions this process
    /// never sees: the next version is taken past the file's revision, so
    /// that a copy that became the home goes on after its old home's.
    behind: AtomicBool,
}

/// Takes the next version inside `taken_in`, whose end keeps the revision's
/// high-water mark, as kv settles it: a version never repeats across a delete,
/// a reopen or a change of home. A write rolled back leaves its number unused,
/// which is all the rule needs.
///
/// The file's revision is read on its writer, so that no commit of an old
/// home is missed: one applied after the read sets `behind` again.
pub(crate) fn next_version(revision: &Revision, taken_in: &Tx<'_>) -> Result<i64> {
    if revision.behind.swap(false, Ordering::SeqCst) {
        let kept = cells::revision(taken_in).inspect_err(|_| revision.behind.store(true, Ordering::SeqCst))?;
        revision.taken.fetch_max(kept, Ordering::SeqCst);
    }
    Ok(revision.taken.fetch_add(1, Ordering::SeqCst) + 1)
}

impl Settle for Kv {
    /// Keeps the revision once a transaction, at its end, where every write
    /// that took a version kept it with its own: a commit of 64 sets ran 64
    /// updates of the one row (issue 7). A transaction that took none skips
    /// it, since every version is taken after the last one kept.
    fn settle(&self, tx: &Tx<'_>, left: Left) -> Result<()> {
        let revision = self.revision.taken.load(Ordering::SeqCst);
        if left != Left::All || revision <= self.kept.load(Ordering::SeqCst) {
            return Ok(());
        }
        cells::keep_revision(tx, revision)?;
        // Ahead of the file if the commit then fails: every version taken
        // next is past it all the same, and one never taken needs no keeping.
        self.kept.store(revision, Ordering::SeqCst);
        Ok(())
    }
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

impl Engine for Kv {
    /// Writes what memory holds, then closes kv.db; the first failure is the
    /// error, and the file closes either way. A file another engine lends is
    /// that engine's to close.
    fn close(&self) -> Result<()> {
        let renewed = if self.file.is_copy() { Ok(()) } else { self.renewals.flush(&self.file).map(drop) };
        let flushed = self.flush_buffers();
        let closed = if self.lent_by.is_none() { self.file.close() } else { Ok(()) };
        // an awaited watch of a config ends once its store closed, rather than wait for ever
        #[cfg(all(feature = "config", feature = "async"))]
        {
            let hubs: Vec<_> = lock(&self.configs).values().filter_map(Weak::upgrade).collect();
            hubs.iter().for_each(|hub| hub.close());
        }
        renewed.and(flushed).and(closed)
    }

    /// Writes what memory holds, so that the copy has every counter and
    /// renewal, then names kv.db; kv in a database's file is the database's
    /// to copy, with its counters written all the same.
    fn snapshot(&self) -> Result<Copying<'_>> {
        self.renewals.flush(&self.file)?;
        self.flush_buffers()?;
        if self.lent_by.is_some() {
            return Ok(Copying::default());
        }
        to_copy(&self.file, FILE, "kv")
    }

    /// A bucket's name and id are its home's, and a copy forgets those it
    /// read once its home's commits arrive, since a name dropped there may
    /// come back as another bucket; and so are the versions its home took.
    fn applied(&self, _: &str) {
        lock(&self.names).clear();
        if self.file.is_copy() {
            self.revision.behind.store(true, Ordering::SeqCst);
        }
    }
}

impl std::fmt::Debug for Kv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Kv({})", self.file.path().display())
    }
}

#[cfg(all(test, feature = "replication", feature = "sql"))]
#[path = "engine_tests.rs"]
mod tests;

/// The buckets kv.db holds and what each is for: values, encrypted values,
/// counters, a rate limit, a quota or once. None while the store has no
/// kv.db, which a read beside would otherwise try to make.
#[cfg(feature = "client")]
pub(crate) fn buckets(store: &Store) -> Result<Vec<(String, String)>> {
    const SELECT_BUCKETS: &str = "select name, role from _tinystore_kv_buckets order by name";
    if !store.dir().join("kv.db").exists() {
        return Ok(Vec::new());
    }
    Kv::of(store)?.file().read(|c| {
        let mut select =
            c.prepare_cached(SELECT_BUCKETS).map_err(|error| crate::sqlite::sql_error("kv: its buckets", error))?;
        let rows = select.query_map([], |row| Ok((row.get(0)?, row.get(1)?)));
        rows.and_then(Iterator::collect).map_err(|error| crate::sqlite::sql_error("kv: its buckets", error))
    })
}
