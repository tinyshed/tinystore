//! What engines use of their store. An application never imports this module:
//! it opens engines, and engines call these.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::sqlite::File;
use crate::store::WeakStore;
use crate::{Error, Result, Store};

/// An engine as its store sees it: something to close at the store's close.
pub trait Engine: Send + Sync + 'static {
    /// Closes the engine. A second call, or a call after the engine closed by
    /// itself, does nothing.
    fn close(&self) -> Result<()>;

    /// The store's clock was moved by hand, as a test moves it: what sleeps
    /// until a time reads the clock again.
    fn clock_moved(&self) {}
}

/// The store's calls for the engines opened against it.
pub trait Host {
    /// The engine of type `E` this store has open, or the one `open` makes and
    /// the store then closes at its own close; every handle shares it.
    fn engine<E: Engine>(&self, open: impl FnOnce(&crate::Store) -> Result<Arc<E>>) -> Result<Arc<E>>;

    /// Closes `engine` when the store closes, the last attached first.
    fn attach(&self, engine: Arc<dyn Engine>) -> Result<()>;

    /// Runs `work` every `period` on the store's background thread, one task at
    /// a time. A failure is logged once and a recovery once. Nothing runs when
    /// the store was opened without background work.
    fn every(&self, name: &str, period: Duration, work: impl FnMut() -> Result<()> + Send + 'static) -> Result<()>;

    /// Claims a file name in the store's directory, so that no other engine
    /// opens the same file; the name is free again once the claim is dropped.
    fn claim(&self, name: &str) -> Result<Claim>;
}

/// What gives a claimed name back to its store.
type Release = Box<dyn FnOnce(&str) + Send + Sync>;

/// A file name in the store's directory that one engine holds.
pub struct Claim {
    name: String,
    path: PathBuf,
    release: Option<Release>,
}

impl Claim {
    pub(crate) fn new(name: &str, path: PathBuf, release: Release) -> Self {
        Self { name: name.to_owned(), path, release: Some(release) }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release(&self.name);
        }
    }
}

impl fmt::Debug for Claim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Claim({})", self.path.display())
    }
}

/// A file one engine opened that others keep their tables in as well: a
/// database lends its file to the buckets and queues opened from it, which
/// share its writer and so commit with its rows. Each engine's tables are
/// named for it and its migrations kept apart, so that they never meet.
///
/// The owner closes the file. The engines kept in it open after the owner,
/// so the store closes them first, while the file is still open.
pub(crate) struct SharedFile {
    file: Arc<File>,
    /// What an error calls the file: `sql app`.
    owner: String,
    /// Weak, as an engine holds it, so that a handle never keeps it open.
    store: WeakStore,
    guests: Mutex<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl SharedFile {
    #[cfg(feature = "sql")]
    pub(crate) fn new(file: File, owner: String, store: &Store) -> SharedFile {
        SharedFile { file: Arc::new(file), owner, store: store.downgrade(), guests: Mutex::new(HashMap::new()) }
    }

    pub(crate) fn file(&self) -> &Arc<File> {
        &self.file
    }

    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    /// The engine of type `E` kept in this file, or the one `open` makes; every
    /// handle opened from the file shares it.
    pub(crate) fn engine<E: Engine>(&self, open: impl FnOnce(&Store) -> Result<Arc<E>>) -> Result<Arc<E>> {
        let store = self.store.upgrade().ok_or_else(|| Error::closed(format!("{}: the store closed", self.owner)))?;
        let mut guests = lock(&self.guests);
        if let Some(engine) = guests.get(&TypeId::of::<E>()) {
            let engine = Arc::clone(engine);
            return Ok(engine.downcast::<E>().expect("an engine is kept under its own type"));
        }
        let engine = open(&store)?;
        store.attach(Arc::clone(&engine) as Arc<dyn Engine>)?;
        guests.insert(TypeId::of::<E>(), Arc::clone(&engine) as Arc<dyn Any + Send + Sync>);
        Ok(engine)
    }
}

impl fmt::Debug for SharedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SharedFile({})", self.owner)
    }
}

/// Where a handle opens: in its engine's own file of the store, `kv.db` or
/// `jobs.db`, or in a file another engine lends, as a database does.
#[derive(Clone, Debug)]
pub(crate) enum Home {
    Store(Store),
    Shared(Arc<SharedFile>),
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
