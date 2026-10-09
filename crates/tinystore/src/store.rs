use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use crate::engine::{Claim, Engine, Host};
use crate::schedule::Scheduler;
use crate::{Clock, Error, ErrorKind, Memory, Result, SystemClock};

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
}

impl Default for Options {
    fn default() -> Self {
        Self { memory: None, clock: None, background: true }
    }
}

impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Options")
            .field("memory", &self.memory)
            .field("clock", &self.clock.as_ref().map(|_| "custom"))
            .field("background", &self.background)
            .finish()
    }
}

/// One directory of engine files, held by one store at a time through its
/// `LOCK`. Engines open against a store and close with it, the last opened
/// first.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
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
    claims: Mutex<HashSet<String>>,
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
            claims: Mutex::new(HashSet::new()),
            closed: AtomicBool::new(false),
            dir,
        };
        Ok(Store { inner: Arc::new(inner) })
    }

    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    pub fn now(&self) -> SystemTime {
        self.inner.clock.now()
    }

    pub fn memory(&self) -> &Arc<Memory> {
        &self.inner.memory
    }

    /// Stops background work, waiting for the task under way, closes every
    /// engine, the last opened first, and lets go of the directory. The first
    /// engine that fails to close is the error; the others still close. A
    /// second call does nothing.
    pub fn close(&self) -> Result<()> {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.inner.scheduler.stop();
        let engines = std::mem::take(&mut *lock(&self.inner.engines));
        let mut first_failure = None;
        for engine in engines.iter().rev() {
            if let Err(error) = engine.close() {
                first_failure.get_or_insert(error);
            }
        }
        lock(&self.inner.open).clear();
        drop(lock(&self.inner.lock).take());
        first_failure.map_or(Ok(()), |error| Err(error.within(self.describe())))
    }

    fn describe(&self) -> String {
        format!("store {}", self.inner.dir.display())
    }

    fn refuse_when_closed(&self, what: &str) -> Result<()> {
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

    fn every(&self, name: &str, period: Duration, work: impl FnMut() -> Result<()> + Send + 'static) -> Result<()> {
        self.refuse_when_closed(name)?;
        self.inner.scheduler.add(name, period, Box::new(work))
    }

    fn claim(&self, name: &str) -> Result<Claim> {
        self.refuse_when_closed(name)?;
        if !lock(&self.inner.claims).insert(name.to_owned()) {
            return Err(Error::new(ErrorKind::InUse, format!("{}: {name} is already open", self.describe())));
        }
        let store = self.clone();
        let release = Box::new(move |name: &str| {
            lock(&store.inner.claims).remove(name);
        });
        Ok(Claim::new(name, self.inner.dir.join(name), release))
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
fn take_lock(dir: &Path) -> Result<File> {
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
