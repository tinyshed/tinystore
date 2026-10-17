//! A node as it runs: the logs of the files it writes, the copies it holds,
//! and the threads of its links, which the store starts at open and stops
//! first at close.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::copy::{self, Copy};
use super::entry::Point;
use super::follow::Upstream;
use super::log::{Acks, Bounds, Log, Notify, segments_in};
use super::snapshot::Source;
use super::{DIR, NODE_LOCAL, Node};
use crate::sqlite::Observe;
use crate::store::WeakStore;
use crate::{Error, ErrorKind, Result, Store};

/// How long opening a file a node copies waits for its first copy.
pub(crate) const READY_WAIT: Duration = Duration::from_secs(30);

pub(crate) struct State {
    pub(crate) dir: PathBuf,
    pub(crate) options: Node,
    /// The store the node is, which runs the writes other nodes pass on.
    store: Mutex<Option<WeakStore>>,
    bounds: Bounds,
    pub(crate) notify: Arc<Notify>,
    /// What a synchronous home's commits wait for.
    pub(crate) acks: Arc<Acks>,
    /// The logs of the files this node writes that are open now.
    logs: Mutex<HashMap<String, Weak<Log>>>,
    /// The copies this node holds whole.
    copies: Mutex<HashMap<String, Arc<Copy>>>,
    copied: Condvar,
    pub(crate) upstream: Upstream,
    pub(crate) stopping: AtomicBool,
    threads: Mutex<Vec<JoinHandle<()>>>,
    /// Every link open, shut down at stop so that its threads stop reading.
    links: Mutex<Vec<TcpStream>>,
    pub(crate) listening: Mutex<Option<SocketAddr>>,
    /// How many snapshots this node began taking, the first copies and those
    /// taken again.
    pub(crate) snapshots: std::sync::atomic::AtomicU64,
    /// A file is being opened that no copy holds yet: the link asks for the
    /// files served now rather than at its next turn.
    pub(crate) wanted: AtomicBool,
}

impl State {
    pub(crate) fn new(dir: &Path, options: Node) -> State {
        State {
            dir: dir.to_owned(),
            options,
            store: Mutex::default(),
            bounds: Bounds::default(),
            notify: Arc::default(),
            acks: Arc::default(),
            logs: Mutex::default(),
            copies: Mutex::default(),
            copied: Condvar::new(),
            upstream: Upstream::default(),
            stopping: AtomicBool::new(false),
            threads: Mutex::default(),
            links: Mutex::default(),
            listening: Mutex::default(),
            snapshots: std::sync::atomic::AtomicU64::new(0),
            wanted: AtomicBool::new(false),
        }
    }

    /// Starts the node's links: a server where it serves, and the link to the
    /// node it takes its files from.
    pub(crate) fn start(self: &Arc<Self>, store: &Store) -> Result<()> {
        *lock(&self.store) = Some(store.downgrade());
        if let Some(address) = &self.options.serve {
            super::serve::start(self, address)?;
        }
        if !self.options.from.is_empty() {
            self.open_copies_held(store)?;
            super::follow::start(self, store);
        }
        Ok(())
    }

    /// Stops every link and waits for its threads; a thread that is the
    /// caller, as one whose store's last handle it held, is left to end.
    pub(crate) fn stop(&self) {
        if self.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(address) = *lock(&self.listening) {
            // wakes the accepting thread, which then sees the node stopping
            let _ = TcpStream::connect_timeout(&address, Duration::from_secs(1));
        }
        for link in lock(&self.links).drain(..) {
            let _ = link.shutdown(std::net::Shutdown::Both);
        }
        self.upstream.stop();
        self.notify.appended();
        let threads: Vec<_> = lock(&self.threads).drain(..).collect();
        for thread in threads {
            if thread.thread().id() != thread::current().id() {
                let _ = thread.join();
            }
        }
        lock(&self.copies).clear();
    }

    /// The store the node is, while it is open.
    pub(crate) fn store(&self) -> Option<Store> {
        lock(&self.store).as_ref().and_then(WeakStore::upgrade)
    }

    pub(crate) fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    pub(crate) fn spawn(&self, name: &str, work: impl FnOnce() + Send + 'static) {
        let thread = thread::Builder::new().name(name.to_owned()).spawn(work);
        match thread {
            Ok(thread) => lock(&self.threads).push(thread),
            Err(error) => tracing::warn!(target: "tinystore", %error, "a node could not start a thread of its links"),
        }
    }

    /// Keeps a link to shut down at stop.
    pub(crate) fn keep_link(&self, link: &TcpStream) {
        if let Ok(link) = link.try_clone() {
            let mut links = lock(&self.links);
            links.retain(|kept| kept.peer_addr().is_ok());
            links.push(link);
        }
    }

    /// The log of the file `name` this node writes, opened for its writer.
    pub(crate) fn open_log(&self, name: &str) -> Result<Arc<dyn Observe>> {
        let mut log = Log::open(self.log_dir(name), self.bounds, Arc::clone(&self.notify))?;
        if self.options.synchronous {
            log = log.synchronous(name, Arc::clone(&self.acks));
        }
        let log = Arc::new(log);
        lock(&self.logs).insert(name.to_owned(), Arc::downgrade(&log));
        Ok(log)
    }

    pub(crate) fn log_dir(&self, name: &str) -> PathBuf {
        self.dir.join(DIR).join(name)
    }

    /// Whether this node takes the file `name` from another rather than
    /// writing it: every file but its own records and metrics when it copies
    /// all, or those it names.
    pub(crate) fn takes(&self, name: &str) -> bool {
        if self.options.from.is_empty() {
            return false;
        }
        // a file whose log is this node's own, as a promoted copy's, it writes
        let dir = self.log_dir(name);
        if !copy::is_copy(&dir) && segments_in(&dir).is_ok_and(|segments| !segments.is_empty())
            || dir.join(super::promote::PROMOTED).exists()
        {
            return false;
        }
        match &self.options.files {
            Some(files) => files.iter().any(|file| file == name),
            None => !NODE_LOCAL.contains(&name),
        }
    }

    /// Waits until the copy of `name` is whole, `READY_WAIT` at most.
    pub(crate) fn wait_ready(&self, name: &str) -> Result<()> {
        let began = Instant::now();
        let deadline = began + READY_WAIT;
        let mut asked_to_make = false;
        let mut copies = lock(&self.copies);
        while !copies.contains_key(name) {
            // a file its home never made would never come: ask the home to make it
            if !asked_to_make && began.elapsed() > Duration::from_millis(500) {
                asked_to_make = true;
                drop(copies);
                if let Err(error) = super::forward::make(self, name) {
                    tracing::debug!(target: "tinystore", file = name, %error, "a node could not ask its home to make a file");
                }
                copies = lock(&self.copies);
                continue;
            }
            let now = Instant::now();
            if now >= deadline || self.stopping() {
                return Err(Error::new(
                    ErrorKind::Unavailable,
                    format!(
                        "{name}: no copy of it has come from {} yet: its home has not made it, or no link reaches it",
                        self.options.from.join(", ")
                    ),
                ));
            }
            self.wanted.store(true, Ordering::SeqCst);
            let turn = (deadline - now).min(Duration::from_millis(250));
            copies = self.copied.wait_timeout(copies, turn).unwrap_or_else(PoisonError::into_inner).0;
        }
        Ok(())
    }

    pub(crate) fn copy(&self, name: &str) -> Option<Arc<Copy>> {
        lock(&self.copies).get(name).cloned()
    }

    /// Keeps a copy that became whole, for its engines and the nodes that
    /// copy it.
    pub(crate) fn keep_copy(&self, name: &str, copy: Arc<Copy>) {
        lock(&self.copies).insert(name.to_owned(), copy);
        self.copied.notify_all();
        self.notify.appended();
    }

    pub(crate) fn drop_copy(&self, name: &str) {
        lock(&self.copies).remove(name);
    }

    /// Opens the copies whole on disk, so that their engines read them while
    /// no link reaches their home.
    fn open_copies_held(&self, store: &Store) -> Result<()> {
        for name in self.files_logged()? {
            if copy::is_ready(&self.log_dir(&name), &self.dir.join(&name)) {
                let copy = Copy::open(store, &name)?;
                self.keep_copy(&name, Arc::new(copy));
            }
        }
        Ok(())
    }

    /// The files this node serves: those it writes that keep a log, and its
    /// copies that are whole.
    pub(crate) fn files_served(&self) -> Result<Vec<String>> {
        let copies = lock(&self.copies).keys().cloned().collect::<HashSet<_>>();
        let mut served = Vec::new();
        for name in self.files_logged()? {
            let dir = self.log_dir(&name);
            if copies.contains(&name) || (!copy::is_copy(&dir) && !segments_in(&dir)?.is_empty()) {
                served.push(name);
            }
        }
        Ok(served)
    }

    /// Every file with a directory in `log/`, by its name in the store's.
    fn files_logged(&self) -> Result<Vec<String>> {
        files_logged_in(&self.dir.join(DIR))
    }

    /// What a snapshot of the file `name` reads, if this node serves it.
    pub(crate) fn source(&self, name: &str) -> Option<Box<dyn Source>> {
        if let Some(copy) = self.copy(name) {
            return Some(Box::new(CopySource(copy)));
        }
        let dir = self.log_dir(name);
        if copy::is_copy(&dir) || !dir.exists() {
            return None;
        }
        let log = lock(&self.logs).get(name).and_then(Weak::upgrade);
        Some(Box::new(HomeSource { path: self.dir.join(name), dir, log }))
    }

    /// Where the log of `name` stands, for a write's caller to wait for.
    pub(crate) fn point_of(&self, name: &str) -> Option<Point> {
        self.source(name).and_then(|source| source.stable().ok())
    }
}

/// Every file with a directory in `logs`, a store's `log/`, by its name in
/// the store's directory.
pub(crate) fn files_logged_in(logs: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    walk(logs, "", &mut names)?;
    names.sort();
    Ok(names)
}

/// Collects the names of the files whose log directories lie under `dir`: a
/// directory holding a segment or a copy's mark is one.
fn walk(dir: &Path, prefix: &str, names: &mut Vec<String>) -> Result<()> {
    let listed = match fs::read_dir(dir) {
        Ok(listed) => listed,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(Error::io(dir.display().to_string(), error)),
    };
    for found in listed {
        let found = found.map_err(|error| Error::io(dir.display().to_string(), error))?;
        let Ok(name) = found.file_name().into_string() else { continue };
        if !found.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = found.path();
        let joined = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        let holds_a_log = copy::is_copy(&path) || !segments_in(&path)?.is_empty();
        if holds_a_log {
            names.push(joined);
        } else {
            walk(&path, &joined, names)?;
        }
    }
    Ok(())
}

/// A file this node writes, its log open with its writer or closed.
struct HomeSource {
    path: PathBuf,
    dir: PathBuf,
    log: Option<Arc<Log>>,
}

impl Source for HomeSource {
    fn path(&self) -> &Path {
        &self.path
    }

    fn stable(&self) -> Result<Point> {
        if let Some(log) = &self.log {
            return Ok(log.stable());
        }
        // With no writer open, a WAL that holds frames may hold commits a
        // process that died made and its log never took: they reach the log
        // when the file's engine opens it.
        let mut wal = self.path.as_os_str().to_owned();
        wal.push("-wal");
        if fs::metadata(PathBuf::from(wal)).is_ok_and(|wal| wal.len() > 0) {
            return Err(Error::new(
                ErrorKind::Unavailable,
                "its WAL holds commits its log may lack until its engine opens it",
            ));
        }
        Log::last_in(&self.dir)?.ok_or_else(|| Error::new(ErrorKind::Unavailable, "its log holds no entry yet"))
    }
}

/// A copy this node holds whole.
struct CopySource(Arc<Copy>);

impl Source for CopySource {
    fn path(&self) -> &Path {
        self.0.path()
    }

    fn stable(&self) -> Result<Point> {
        Ok(self.0.point())
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
