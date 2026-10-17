use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
#[cfg(any(feature = "kv", feature = "jobs", feature = "sql", feature = "records", feature = "replication"))]
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

#[cfg(feature = "kv")]
use crate::encryption::{self, EncryptionKey};
use crate::engine::{Claim, Engine, Host, Wake};
use crate::schedule::Scheduler;
#[cfg(any(feature = "kv", feature = "jobs"))]
use crate::sqlite::File as EngineFile;
use crate::{Clock, Durability, Error, ErrorKind, Memory, Result, SystemClock};

/// How a store opens.
pub struct Options {
    /// Bytes all engines' work may hold at once; `None` leaves each engine to
    /// its own per-call bounds.
    pub memory: Option<u64>,
    /// Where the store and every engine read the time; the system's clock when
    /// `None`.
    pub clock: Option<Arc<dyn Clock>>,
    /// Whether the store runs periodic work, expiry and maintenance among it.
    /// Tests turn it off and run each engine's maintenance themselves.
    pub background: bool,
    /// How far a commit of the store's files goes before it returns: `kv.db`,
    /// `jobs.db`, and every database that does not say its own. `None` leaves
    /// each engine its own, which for these is [`Durability::Full`].
    pub durability: Option<Durability>,
    /// Bytes of the disk the store keeps free: a file that would leave less is
    /// a `Limit` error and leaves nothing. 1 GiB unless said; 0 keeps none.
    pub keep_free: u64,
    /// The file of the store's encryption key, which seals the values of its
    /// encrypted buckets: 64 hex digits, as `openssl rand -hex 32` writes. The
    /// store reads it and never makes it. `None` is `encryption.key` in the
    /// store's directory, which the store makes the first time it needs one.
    pub encryption_key_file: Option<PathBuf>,
    /// Makes the store a node, which keeps a log of each file it writes for
    /// other nodes to copy; `None` is a store alone.
    #[cfg(feature = "replication")]
    pub node: Option<crate::replication::Node>,
}

/// The disk a store keeps free unless its options say otherwise.
pub(crate) const KEEP_FREE: u64 = 1 << 30;

impl Default for Options {
    fn default() -> Self {
        Self {
            memory: None,
            clock: None,
            background: true,
            durability: None,
            keep_free: KEEP_FREE,
            encryption_key_file: None,
            #[cfg(feature = "replication")]
            node: None,
        }
    }
}

impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let f = &mut f.debug_struct("Options");
        let f = f
            .field("memory", &self.memory)
            .field("clock", &self.clock.as_ref().map(|_| "custom"))
            .field("background", &self.background)
            .field("durability", &self.durability)
            .field("keep_free", &self.keep_free)
            .field("encryption_key_file", &self.encryption_key_file);
        #[cfg(feature = "replication")]
        let f = f.field("node", &self.node);
        f.finish()
    }
}

/// One directory of engine files, held by one store at a time through its
/// `LOCK`. Engines open against a store and close with it, the last opened
/// first: at `close`, or when the last clone of the store is dropped.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

/// A store that its engines reach without keeping it open: an engine holds
/// this, so that dropping the application's last handle closes the store.
#[derive(Clone, Debug)]
#[cfg(any(feature = "kv", feature = "jobs", feature = "sql", feature = "records", feature = "replication"))]
pub(crate) struct WeakStore(
    #[cfg_attr(
        not(any(feature = "kv", feature = "jobs", feature = "records", feature = "replication")),
        expect(dead_code, reason = "a database's file holds its store for its guests, and sql alone has none")
    )]
    Weak<Inner>,
);

#[cfg(any(feature = "kv", feature = "jobs", feature = "sql", feature = "records", feature = "replication"))]
impl WeakStore {
    #[cfg_attr(
        not(any(feature = "kv", feature = "jobs", feature = "records", feature = "replication")),
        expect(dead_code, reason = "a database's file holds its store for its guests, and sql alone has none")
    )]
    pub(crate) fn upgrade(&self) -> Option<Store> {
        self.0.upgrade().map(|inner| Store { inner })
    }
}

struct Inner {
    dir: PathBuf,
    lock: Mutex<Option<File>>,
    clock: Arc<dyn Clock>,
    memory: Arc<Memory>,
    scheduler: Scheduler,
    engines: Mutex<Vec<Arc<dyn Engine>>>,
    /// The engine of each kind this store has open, which every handle of that
    /// engine shares: one `kv.db` writer however many buckets open.
    open: Mutex<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    /// Shared with each claim's release, which must not keep the store open.
    claims: Arc<Mutex<HashSet<String>>>,
    /// The engines' own files, kv.db and jobs.db, which the store's
    /// transaction is a transaction of one of.
    #[cfg(any(feature = "kv", feature = "jobs"))]
    files: Mutex<Vec<Arc<EngineFile>>>,
    #[cfg_attr(
        not(any(
            feature = "kv",
            feature = "jobs",
            feature = "sql",
            feature = "blobs",
            feature = "records",
            feature = "metrics",
            feature = "search",
            feature = "backup"
        )),
        expect(dead_code, reason = "only an engine of files reads it, and channels keep none")
    )]
    durability: Option<Durability>,
    keep_free: u64,
    #[cfg(feature = "kv")]
    encryption_key_file: Option<PathBuf>,
    /// The key, once a handle needed it: a store that seals nothing reads no
    /// key and makes none.
    #[cfg(feature = "kv")]
    encryption_key: Mutex<Option<Arc<EncryptionKey>>>,
    /// The node the store is, its logs, copies and links; `None` in a store
    /// alone.
    #[cfg(feature = "replication")]
    node: Option<Arc<crate::replication::State>>,
    closed: AtomicBool,
}

impl Store {
    /// Opens the store in `dir`, creating the directory when it is missing.
    /// Another store holding the directory, in this process or another, makes
    /// it an `InUse` error.
    pub fn open(dir: impl AsRef<Path>, options: Options) -> Result<Store> {
        let dir = absolute(dir.as_ref())?;
        let lock = take_lock(&dir)?;
        let inner = Inner {
            lock: Mutex::new(Some(lock)),
            clock: options.clock.unwrap_or_else(|| Arc::new(SystemClock)),
            memory: Memory::new(options.memory.unwrap_or(u64::MAX)),
            scheduler: Scheduler::new(options.background),
            engines: Mutex::new(Vec::new()),
            open: Mutex::new(HashMap::new()),
            claims: Arc::default(),
            #[cfg(any(feature = "kv", feature = "jobs"))]
            files: Mutex::new(Vec::new()),
            durability: options.durability,
            keep_free: options.keep_free,
            #[cfg(feature = "kv")]
            encryption_key_file: options.encryption_key_file,
            #[cfg(feature = "kv")]
            encryption_key: Mutex::new(None),
            #[cfg(feature = "replication")]
            node: options.node.map(|node| crate::replication::State::new(&dir, node).map(Arc::new)).transpose()?,
            closed: AtomicBool::new(false),
            dir,
        };
        let store = Store { inner: Arc::new(inner) };
        #[cfg(feature = "replication")]
        if let Some(node) = &store.inner.node {
            node.start(&store)?;
        }
        Ok(store)
    }

    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    /// Opens an engine's SQLite file, `name` inside the store's directory, its
    /// commits logged when the store is a node.
    #[cfg_attr(
        not(any(
            feature = "kv",
            feature = "jobs",
            feature = "sql",
            feature = "blobs",
            feature = "records",
            feature = "metrics",
            feature = "search"
        )),
        expect(dead_code, reason = "only an engine of files opens one, and channels keep none")
    )]
    pub(crate) fn open_file(&self, name: &str, config: crate::sqlite::Config) -> Result<crate::sqlite::File> {
        let path = self.inner.dir.join(name);
        #[cfg(feature = "replication")]
        if let Some(node) = &self.inner.node {
            if node.takes(name) {
                node.wait_ready(name).map_err(|error| error.within(self.describe()))?;
                return crate::sqlite::File::open_copy(path, config);
            }
            if crate::replication::is_copy(&node.log_dir(name)) {
                return crate::sqlite::File::open_copy(path, config);
            }
            return crate::sqlite::File::open_observed(path, config, Some(node.open_log(name)?));
        }
        crate::sqlite::File::open(path, config)
    }

    /// The directory of the log of the store's file `name`; a store that is
    /// no node keeps none.
    #[cfg(feature = "replication")]
    pub(crate) fn log_of(&self, name: &str) -> Result<PathBuf> {
        let node = self.inner.node.as_ref().ok_or_else(|| {
            Error::invalid(format!("{}: a store that is no node keeps no log: open it with a node", self.describe()))
        })?;
        Ok(node.log_dir(name))
    }

    /// The node the store is, if it is one.
    #[cfg(feature = "replication")]
    pub(crate) fn node(&self) -> Option<&Arc<crate::replication::State>> {
        self.inner.node.as_ref()
    }

    /// What a node's links wait on for the next entry of its logs.
    #[cfg(feature = "replication")]
    pub(crate) fn node_notify(&self) -> Arc<crate::replication::Notify> {
        self.inner.node.as_ref().map_or_else(Arc::default, |node| Arc::clone(&node.notify))
    }

    /// Tells every engine open that a commit of its home was applied to the
    /// copy `name`.
    #[cfg(feature = "replication")]
    pub(crate) fn applied(&self, name: &str) {
        let engines = lock(&self.inner.engines).clone();
        for engine in engines {
            engine.applied(name);
        }
    }

    /// What the store's options said of its files' commits, for an engine
    /// that opens one.
    #[cfg_attr(
        not(any(
            feature = "kv",
            feature = "jobs",
            feature = "sql",
            feature = "blobs",
            feature = "records",
            feature = "metrics",
            feature = "search",
            feature = "backup"
        )),
        expect(dead_code, reason = "only an engine of files reads it, and channels keep none")
    )]
    pub(crate) fn durability(&self) -> Option<Durability> {
        self.inner.durability
    }

    /// The file its options named for its encryption key, if they named one.
    #[cfg(all(feature = "kv", any(feature = "wire", feature = "backup")))]
    pub(crate) fn encryption_key_file(&self) -> Option<&Path> {
        self.inner.encryption_key_file.as_deref()
    }

    /// The bytes of the disk the store keeps free, as its options said.
    pub fn keep_free(&self) -> u64 {
        self.inner.keep_free
    }

    /// The store's encryption key: the one in the file its options named, or
    /// in `encryption.key` of its directory, which the store makes when it is
    /// not there. For a bucket that holds nothing sealed yet.
    #[cfg(feature = "kv")]
    pub(crate) fn encryption_key(&self) -> Result<Arc<EncryptionKey>> {
        self.read_encryption_key(true)
    }

    /// The store's encryption key, which is there already: a key lost is not
    /// replaced, since values sealed with it open with no other.
    #[cfg(feature = "kv")]
    pub(crate) fn encryption_key_there(&self) -> Result<Arc<EncryptionKey>> {
        self.read_encryption_key(false)
    }

    #[cfg(feature = "kv")]
    fn read_encryption_key(&self, make: bool) -> Result<Arc<EncryptionKey>> {
        self.refuse_when_closed("its encryption key")?;
        let mut kept = lock(&self.inner.encryption_key);
        if let Some(key) = &*kept {
            return Ok(Arc::clone(key));
        }
        let own = self.inner.dir.join(encryption::FILE);
        let key = Arc::new(match &self.inner.encryption_key_file {
            Some(file) => EncryptionKey::read(file)?,
            None if make => EncryptionKey::read_or_make(&own)?,
            None => EncryptionKey::read(&own)?,
        });
        *kept = Some(Arc::clone(&key));
        Ok(key)
    }

    pub fn now(&self) -> SystemTime {
        self.inner.clock.now()
    }

    /// Tells every engine that a test moved the store's clock by hand, so
    /// that what sleeps until a time reads it again: a worker waiting for a
    /// job due in an hour runs it once the clock is an hour on.
    pub fn clock_moved(&self) {
        let engines = lock(&self.inner.engines).clone();
        for engine in engines {
            engine.clock_moved();
        }
    }

    pub fn memory(&self) -> &Arc<Memory> {
        &self.inner.memory
    }

    /// Stops background work, waiting for the task under way, closes every
    /// engine, the last opened first, and lets go of the directory. The first
    /// engine that fails to close is the error; the others still close. A
    /// second call does nothing, and neither does the last clone's drop after.
    pub fn close(&self) -> Result<()> {
        self.inner.close()
    }

    #[cfg_attr(
        not(any(
            feature = "kv",
            feature = "jobs",
            feature = "sql",
            feature = "blobs",
            feature = "records",
            feature = "metrics",
            feature = "search",
            feature = "backup"
        )),
        expect(dead_code, reason = "only an engine of files reads the clock, and channels keep none")
    )]
    pub(crate) fn clock(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.inner.clock)
    }

    #[cfg(any(feature = "kv", feature = "sql", feature = "records", feature = "replication"))]
    pub(crate) fn downgrade(&self) -> WeakStore {
        WeakStore(Arc::downgrade(&self.inner))
    }

    /// Keeps an engine's own file among those [`Store::tx`] may be a
    /// transaction of.
    #[cfg(any(feature = "kv", feature = "jobs"))]
    pub(crate) fn keep_file(&self, file: &Arc<EngineFile>) {
        lock(&self.inner.files).push(Arc::clone(file));
    }

    #[cfg(any(feature = "kv", feature = "jobs"))]
    pub(crate) fn engine_files(&self) -> Vec<Arc<EngineFile>> {
        lock(&self.inner.files).clone()
    }

    /// Every engine the store has open, and the names they hold in its
    /// directory: what a snapshot asks to copy their own.
    #[cfg(feature = "backup")]
    pub(crate) fn engines_open(&self) -> Result<EnginesOpen> {
        self.refuse_when_closed("a snapshot")?;
        let _opening = lock(&self.inner.open);
        Ok((lock(&self.inner.engines).clone(), lock(&self.inner.claims).clone()))
    }

    fn describe(&self) -> String {
        self.inner.describe()
    }

    pub(crate) fn refuse_when_closed(&self, what: &str) -> Result<()> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(Error::closed(format!("{}: {what}", self.describe())));
        }
        Ok(())
    }
}

impl Host for Store {
    fn engine<E: Engine>(&self, open: impl FnOnce(&Store) -> Result<Arc<E>>) -> Result<Arc<E>> {
        self.refuse_when_closed("an engine opening")?;
        let mut engines = lock(&self.inner.open);
        if let Some(engine) = engines.get(&TypeId::of::<E>()) {
            let engine = Arc::clone(engine);
            return Ok(engine.downcast::<E>().expect("an engine is kept under its own type"));
        }
        let engine = open(self)?;
        self.attach(Arc::clone(&engine) as Arc<dyn Engine>)?;
        engines.insert(TypeId::of::<E>(), Arc::clone(&engine) as Arc<dyn Any + Send + Sync>);
        Ok(engine)
    }

    fn attach(&self, engine: Arc<dyn Engine>) -> Result<()> {
        self.refuse_when_closed("an engine opening")?;
        lock(&self.inner.engines).push(engine);
        Ok(())
    }

    fn every(&self, name: &str, period: Duration, work: impl FnMut() -> Result<()> + Send + 'static) -> Result<Wake> {
        self.refuse_when_closed(name)?;
        self.inner.scheduler.add(name, period, Box::new(work))
    }

    fn claim(&self, name: &str) -> Result<Claim> {
        self.refuse_when_closed(name)?;
        if !lock(&self.inner.claims).insert(name.to_owned()) {
            return Err(Error::new(ErrorKind::InUse, format!("{}: {name} is already open", self.describe())));
        }
        let claims = Arc::clone(&self.inner.claims);
        let release = Box::new(move |name: &str| {
            lock(&claims).remove(name);
        });
        Ok(Claim::new(name, self.inner.dir.join(name), release))
    }
}

impl Inner {
    fn close(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        #[cfg(feature = "replication")]
        if let Some(node) = &self.node {
            node.stop();
        }
        self.scheduler.stop();
        let engines = std::mem::take(&mut *lock(&self.engines));
        let mut first_failure = None;
        for engine in engines.iter().rev() {
            if let Err(error) = engine.close() {
                first_failure.get_or_insert(error);
            }
        }
        lock(&self.open).clear();
        #[cfg(any(feature = "kv", feature = "jobs"))]
        lock(&self.files).clear();
        drop(lock(&self.lock).take());
        first_failure.map_or(Ok(()), |error| Err(error.within(self.describe())))
    }

    fn describe(&self) -> String {
        format!("store {}", self.dir.display())
    }
}

impl Drop for Inner {
    /// Closes a store its application dropped without closing, so that what
    /// engines hold in memory is written and the directory is let go.
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            tracing::warn!(target: "tinystore", %error, "a store dropped without close did not close cleanly");
        }
    }
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

fn absolute(dir: &Path) -> Result<PathBuf> {
    if dir.as_os_str().is_empty() {
        return Err(Error::invalid("store: a directory with no name"));
    }
    let dir = std::path::absolute(dir).map_err(|error| Error::io(format!("store {}", dir.display()), error))?;
    fs::create_dir_all(&dir).map_err(|error| Error::io(format!("store {}", dir.display()), error))?;
    Ok(dir)
}

/// Takes the directory's `LOCK`. The lock belongs to the open file, so a
/// second store in the same process is refused like one in another process,
/// and the operating system lets go of it if the process dies.
/// The engines a store has open, and the names they hold in its directory.
#[cfg(feature = "backup")]
pub(crate) type EnginesOpen = (Vec<Arc<dyn Engine>>, HashSet<String>);

pub(crate) fn take_lock(dir: &Path) -> Result<File> {
    let path = dir.join("LOCK");
    let what = || format!("store {}: LOCK", dir.display());
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| Error::io(what(), error))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => {
            Err(Error::new(ErrorKind::InUse, format!("{}: another store holds it", what())))
        }
        Err(TryLockError::Error(error)) => Err(Error::io(what(), error)),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TestClock;

    struct Recorder {
        name: &'static str,
        closed: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Engine for Recorder {
        fn close(&self) -> Result<()> {
            lock(&self.closed).push(self.name);
            Ok(())
        }
    }

    #[test]
    fn a_directory_opens_in_one_store_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let first = Store::open(dir.path(), Options::default()).unwrap();
        let error = Store::open(dir.path(), Options::default()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InUse, "{error}");

        first.close().unwrap();
        let second = Store::open(dir.path(), Options::default()).unwrap();
        second.close().unwrap();
    }

    #[test]
    fn a_missing_directory_is_created() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("a").join("b");
        let store = Store::open(&dir, Options::default()).unwrap();
        assert!(dir.join("LOCK").is_file());
        store.close().unwrap();
    }

    #[test]
    fn engines_close_the_last_opened_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options::default()).unwrap();
        let closed = Arc::new(Mutex::new(Vec::new()));
        for name in ["kv", "jobs", "records"] {
            let engine = Recorder { name, closed: Arc::clone(&closed) };
            store.attach(Arc::new(engine)).unwrap();
        }
        store.close().unwrap();
        assert_eq!(*lock(&closed), ["records", "jobs", "kv"]);
    }

    #[test]
    fn a_file_is_claimed_by_one_engine_until_it_lets_go() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options::default()).unwrap();
        let claim = store.claim("kv.db").unwrap();
        assert_eq!(claim.path(), store.dir().join("kv.db"));
        assert_eq!(store.claim("kv.db").unwrap_err().kind(), ErrorKind::InUse);
        drop(claim);
        store.claim("kv.db").unwrap();
        store.close().unwrap();
    }

    #[test]
    fn a_store_dropped_without_close_closes_its_engines_and_lets_go() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options::default()).unwrap();
        let closed = Arc::new(Mutex::new(Vec::new()));
        store.attach(Arc::new(Recorder { name: "kv", closed: Arc::clone(&closed) })).unwrap();
        let clone = store.clone();
        drop(store);
        assert!(lock(&closed).is_empty(), "a clone still holds it open");
        drop(clone);
        assert_eq!(*lock(&closed), ["kv"]);
        Store::open(dir.path(), Options::default()).unwrap().close().unwrap();
    }

    #[test]
    fn a_closed_store_opens_nothing_more() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options::default()).unwrap();
        store.close().unwrap();
        assert_eq!(store.claim("kv.db").unwrap_err().kind(), ErrorKind::Closed);
        assert_eq!(store.every("kv: expiry", Duration::from_secs(1), || Ok(())).unwrap_err().kind(), ErrorKind::Closed);
        store.close().unwrap();
    }

    #[test]
    fn the_store_reads_the_clock_it_was_given() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::new(SystemTime::UNIX_EPOCH + Duration::from_secs(60)));
        let options = Options { clock: Some(clock.clone()), ..Options::default() };
        let store = Store::open(dir.path(), options).unwrap();
        clock.advance(Duration::from_secs(1));
        assert_eq!(crate::unix_millis(store.now()), 61_000);
        store.close().unwrap();
    }
}
