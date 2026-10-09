//! What engines use of their store. An application never imports this module:
//! it opens engines, and engines call these.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::Result;

/// An engine as its store sees it: something to close at the store's close.
pub trait Engine: Send + Sync + 'static {
    /// Closes the engine. A second call, or a call after the engine closed by
    /// itself, does nothing.
    fn close(&self) -> Result<()>;
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
