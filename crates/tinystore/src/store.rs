use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
#[cfg(any(
    feature = "kv",
    feature = "jobs",
    feature = "sql",
    feature = "records",
    feature = "metrics",
    feature = "replication"
))]
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

#[cfg(feature = "kv")]
use crate::encryption::{self, EncryptionKey};
use crate::engine::{Claim, Engine, Host, Wake};
use crate::probe::Probe;
use crate::schedule::Scheduler;
#[cfg(any(feature = "kv", feature = "jobs"))]
use crate::sqlite::File as EngineFile;
use crate::sync::lock;
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
    /// Measures the store into its own `metrics.db`, under names that start
    /// with `tinystore_`: its commits, their waits, its files, its memory and
    /// the machine it runs on, as docs/guides/metrics/self-metrics.md lists them. `None`
    /// unless asked for.
    #[cfg(feature = "metrics")]
    pub self_metrics: Option<crate::metrics::SelfMetrics>,
    /// The metrics series the store holds at most: a new series past them is
    /// a `Limit` error, and a series kept no longer gives its place back.
    /// 100,000 unless said; docs/guides/metrics/ingesting.md says what one costs.
    #[cfg(feature = "metrics")]
    pub max_series: u64,
}

/// The disk a store keeps free unless its options say otherwise.
pub(crate) const KEEP_FREE: u64 = 1 << 30;

/// The metrics series a store holds unless its options say otherwise.
#[cfg(feature = "metrics")]
pub(crate) const MAX_SERIES: u64 = 100_000;

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
            #[cfg(feature = "metrics")]
            self_metrics: None,
            #[cfg(feature = "metrics")]
            max_series: MAX_SERIES,
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
        #[cfg(feature = "metrics")]
        let f = f.field("self_metrics", &self.self_metrics).field("max_series", &self.max_series);
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
#[cfg(any(
    feature = "kv",
    feature = "jobs",
    feature = "sql",
    feature = "records",
    feature = "metrics",
    feature = "replication"
))]
pub(crate) struct WeakStore(
    #[cfg_attr(
        not(any(
            feature = "kv",
            feature = "jobs",
            feature = "records",
            feature = "metrics",
            feature = "replication",
            feature = "search"
        )),
        expect(dead_code, reason = "a database's file holds its store for its guests, and sql alone has none")
    )]
    Weak<Inner>,
);

#[cfg(any(
    feature = "kv",
    feature = "jobs",
    feature = "sql",
    feature = "records",
    feature = "metrics",
    feature = "replication"
))]
impl WeakStore {
    #[cfg_attr(
        not(any(
            feature = "kv",
            feature = "jobs",
            feature = "records",
            feature = "metrics",
            feature = "replication",
            feature = "search"
        )),
        expect(dead_code, reason = "a database's file holds its store for its guests, and sql alone has none")
    )]
    pub(crate) fn upgrade(&self) -> Option<Store> {
        self.0.upgrade().map(|inner| Store { inner })
    }
}

/// Engines open on a file, each by its type.
pub(crate) type Engines = HashMap<TypeId, Arc<dyn Any + Send + Sync>>;

struct Inner {
    dir: PathBuf,
    lock: Mutex<Option<File>>,
    clock: Arc<dyn Clock>,
    memory: Arc<Memory>,
    /// What the store measures of itself goes through, heard only when it
    /// was opened with `self_metrics`.
    probe: Arc<Probe>,
    scheduler: Scheduler,
    engines: Mutex<Vec<Arc<dyn Engine>>>,
    /// The engine of each kind this store has open, which every handle of that
    /// engine shares: one `kv.db` writer however many buckets open.
    open: Mutex<Engines>,
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
    #[cfg(feature = "metrics")]
    max_series: u64,
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
    /// Opened beside the process that holds the directory: it reads only.
    beside: bool,
    closed: AtomicBool,
}

impl Store {
    /// Opens the store in `dir`, creating the directory when it is missing.
    /// Another store holding the directory, in this process or another, makes
    /// it an `InUse` error.
    pub fn open(dir: impl AsRef<Path>, options: Options) -> Result<Store> {
        let dir = absolute(dir.as_ref())?;
        let lock = take_lock(&dir)?;
        Self::open_with(dir, options, Some(lock))
    }

    /// Opens the store in `dir` to read beside the process that holds it, a
    /// server or a program: no lock taken, no background work, each file read
    /// through readers alone, none written and none checkpointed. A write is
    /// refused, `Invalid`, and a file the store does not hold is `NotFound`
    /// rather than made. What the holder commits meanwhile, a read sees.
    pub fn open_beside(dir: impl AsRef<Path>) -> Result<Store> {
        if !dir.as_ref().is_dir() {
            return Err(Error::not_found(format!("store {}: no such directory", dir.as_ref().display())));
        }
        let dir = absolute(dir.as_ref())?;
        Self::open_with(dir, Options { background: false, ..Options::default() }, None)
    }

    fn open_with(dir: PathBuf, options: Options, lock: Option<File>) -> Result<Store> {
        #[cfg(feature = "metrics")]
        if options.max_series == 0 {
            return Err(Error::invalid(format!("store {}: a max_series of 0, which holds no series", dir.display())));
        }
        let beside = lock.is_none();
        #[cfg(feature = "metrics")]
        let self_metrics = options.self_metrics.filter(|_| !beside);
        let probe = Arc::new(Probe::default());
        let inner = Inner {
            lock: Mutex::new(lock),
            clock: options.clock.unwrap_or_else(|| Arc::new(SystemClock)),
            memory: Memory::new(options.memory.unwrap_or(u64::MAX)),
            scheduler: Scheduler::new(options.background, Arc::clone(&probe)),
            probe,
            engines: Mutex::new(Vec::new()),
            open: Mutex::new(HashMap::new()),
            claims: Arc::default(),
            #[cfg(any(feature = "kv", feature = "jobs"))]
            files: Mutex::new(Vec::new()),
            durability: options.durability,
            keep_free: options.keep_free,
            #[cfg(feature = "metrics")]
            max_series: options.max_series,
            #[cfg(feature = "kv")]
            encryption_key_file: options.encryption_key_file,
            #[cfg(feature = "kv")]
            encryption_key: Mutex::new(None),
            #[cfg(feature = "replication")]
            node: options
                .node
                .map(|node| crate::replication::State::new(&dir, node, options.keep_free).map(Arc::new))
                .transpose()?,
            beside,
            closed: AtomicBool::new(false),
            dir,
        };
        let store = Store { inner: Arc::new(inner) };
        #[cfg(feature = "replication")]
        if let Some(node) = &store.inner.node {
            node.start(&store)?;
        }
        // before any engine, so that every file is measured from its first commit
        #[cfg(feature = "metrics")]
        if let Some(asked) = self_metrics {
            crate::metrics::measure_the_store(&store, asked)?;
        }
        Ok(store)
    }

    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    /// Whether the store was opened beside its holder, to read only.
    pub fn is_beside(&self) -> bool {
        self.inner.beside
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
        let probe = Some(self.inner.probe.for_file(crate::probe::label(name)));
        let config = crate::sqlite::Config { memory: Some(Arc::clone(&self.inner.memory)), probe, ..config };
        if self.inner.beside {
            if !path.exists() {
                return Err(Error::not_found(format!("{}: it holds no {name}", self.describe())));
            }
            return crate::sqlite::File::open_copy(path, config);
        }
        #[cfg(feature = "replication")]
        if let Some(node) = &self.inner.node {
            // the route is chosen and the file opened on one side of a change of
            // the route: a takeover, a hand-over or a snapshot taken again
            let _opening = node.wait_to_open(name).map_err(|error| error.within(self.describe()))?;
            if node.is_new(name) && node.uses_catalog() {
                return self.open_new_in_the_catalog(node, name, config);
            }
            if node.takes(name) {
                node.wait_ready(name).map_err(|error| error.within(self.describe()))?;
                return Ok(node.opened(name, crate::sqlite::File::open_copy(path, config)?));
            }
            if crate::replication::is_copy(&node.log_dir(name)) {
                return Ok(node.opened(name, crate::sqlite::File::open_copy(path, config)?));
            }
            let file = crate::sqlite::File::open_observed(path, config, Some(node.open_log(name)?))?;
            return Ok(node.opened(name, file));
        }
        crate::sqlite::File::open(path, config)
    }

    /// Opens a file no node of the store's held before it: written here once
    /// this node's claim in the catalog won, copied from its home otherwise.
    #[cfg(feature = "replication")]
    fn open_new_in_the_catalog(
        &self,
        node: &Arc<crate::replication::State>,
        name: &str,
        config: crate::sqlite::Config,
    ) -> Result<crate::sqlite::File> {
        let path = self.inner.dir.join(name);
        match node.claim(self, name).map_err(|error| error.within(self.describe()))? {
            crate::replication::Claimed::Here => {
                let file = crate::sqlite::File::open_observed(path, config, Some(node.open_log(name)?))?;
                return Ok(node.opened(name, file));
            }
            crate::replication::Claimed::At(address) => node.take_directly(self, name, &address)?,
            crate::replication::Claimed::From if !node.takes(name) => {
                return Err(Error::new(
                    crate::ErrorKind::Unavailable,
                    format!("{name}: its home serves nothing other nodes reach, and this node takes from none"),
                )
                .within(self.describe()));
            }
            crate::replication::Claimed::From => {}
        }
        node.wait_ready(name).map_err(|error| error.within(self.describe()))?;
        Ok(node.opened(name, crate::sqlite::File::open_copy(path, config)?))
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

    /// The bounds of a node's logs, a copy's as a home's, so that the disk
    /// the store keeps free reaches both.
    #[cfg(feature = "replication")]
    pub(crate) fn node_bounds(&self) -> crate::replication::Bounds {
        self.inner.node.as_ref().map_or_else(Default::default, |node| node.bounds())
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

    /// The metrics series the store holds at most, as its options said.
    #[cfg(feature = "metrics")]
    pub fn max_series(&self) -> u64 {
        self.inner.max_series
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
        // a store opened beside its holder never makes the key file: it reads
        // the key there, as any other file
        let key = Arc::new(match &self.inner.encryption_key_file {
            Some(file) => EncryptionKey::read(file)?,
            None if make && !self.inner.beside => EncryptionKey::read_or_make(&own)?,
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

    /// What the store measures of itself goes through.
    #[allow(dead_code, reason = "each engine reports what it measures: a store of none of them reads it not")]
    pub(crate) fn probe(&self) -> &Arc<Probe> {
        &self.inner.probe
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

    #[cfg(any(feature = "kv", feature = "sql", feature = "records", feature = "metrics", feature = "replication"))]
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

    /// The engine of type `E` that `kept` holds, or the one `open` makes,
    /// attached to the store and kept there.
    pub(crate) fn kept_or_opened<E: Engine>(
        &self,
        kept: &Mutex<Engines>,
        open: impl FnOnce(&Store) -> Result<Arc<E>>,
    ) -> Result<Arc<E>> {
        let mut kept = lock(kept);
        if let Some(engine) = kept.get(&TypeId::of::<E>()) {
            let engine = Arc::clone(engine);
            return Ok(engine.downcast::<E>().expect("an engine is kept under its own type"));
        }
        let engine = open(self)?;
        self.attach(Arc::clone(&engine) as Arc<dyn Engine>)?;
        kept.insert(TypeId::of::<E>(), Arc::clone(&engine) as Arc<dyn Any + Send + Sync>);
        Ok(engine)
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
        self.kept_or_opened(&self.inner.open, |store| crate::sqlite::own_work(|| open(store)))
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

/// The engines a store has open, and the names they hold in its directory.
#[cfg(feature = "backup")]
pub(crate) type EnginesOpen = (Vec<Arc<dyn Engine>>, HashSet<String>);

/// Takes the directory's `LOCK`. The lock belongs to the open file, so a
/// second store in the same process is refused like one in another process,
/// and the operating system lets go of it if the process dies.
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
        Err(TryLockError::WouldBlock) => Err(Error::new(ErrorKind::InUse, held_by(dir, what()))),
        Err(TryLockError::Error(error)) => Err(Error::io(what(), error)),
    }
}

/// Who holds the `LOCK`, as far as the directory says: a server publishes
/// `server/SERVE` while it serves, and a sidecar serves on after its last
/// client closed, so a program that opens the directory embedded next is
/// told how to let it go.
fn held_by(dir: &Path, what: String) -> String {
    if dir.join("server").join("SERVE").is_file() {
        let dir = dir.display();
        format!("{what}: a tinystore server holds it; connect to it, or stop it first with `tinystore stop {dir}`")
    } else {
        format!("{what}: another store holds it")
    }
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

    // contract: STORE-01
    #[test]
    #[cfg(all(feature = "kv", feature = "jobs", feature = "replication"))]
    fn a_store_opened_beside_its_holder_reads_what_it_commits_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let holder = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        let sessions = holder.bucket::<String>("sessions").open().unwrap();
        sessions.set("a", &"first".to_owned()).unwrap();

        let beside = Store::open_beside(dir.path()).unwrap();
        assert!(beside.is_beside());
        let read = beside.bucket::<String>("sessions").open().unwrap();
        assert_eq!(read.get("a").unwrap().as_deref(), Some("first"));
        sessions.set("b", &"committed after".to_owned()).unwrap();
        assert_eq!(read.get("b").unwrap().as_deref(), Some("committed after"), "a read sees the holder's commits");
        let refused = read.set("c", &"no".to_owned()).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
        let missing = beside.queue::<String>("mail").open().unwrap_err();
        assert_eq!(missing.kind(), ErrorKind::NotFound, "a file it does not hold is not made: {missing}");
        assert!(!dir.path().join("jobs.db").exists());
        beside.close().unwrap();
        sessions.set("d", &"the holder writes on".to_owned()).unwrap();
        holder.close().unwrap();
        assert!(Store::open_beside(dir.path().join("nowhere")).is_err());
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
    fn a_directory_a_server_holds_says_how_to_let_it_go() {
        let dir = tempfile::tempdir().unwrap();
        let server = Store::open(dir.path(), Options::default()).unwrap();
        std::fs::create_dir(dir.path().join("server")).unwrap();
        std::fs::write(dir.path().join("server").join("SERVE"), "{}").unwrap();
        let error = Store::open(dir.path(), Options::default()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InUse, "{error}");
        assert!(error.to_string().contains("a tinystore server holds it"), "{error}");
        assert!(error.to_string().contains(&format!("tinystore stop {}", dir.path().display())), "{error}");
        server.close().unwrap();
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

    // contract: STORE-04
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
