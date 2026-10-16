//! What engines use of their store. An application never imports this module:
//! it opens engines, and engines call these.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::Result;
pub use crate::schedule::Wake;

// a database's file lent to kv's and jobs' handles, and where a handle opens
#[cfg(any(feature = "kv", feature = "jobs", feature = "sql"))]
mod shared;
#[cfg(any(feature = "kv", feature = "jobs"))]
pub(crate) use shared::Home;
#[cfg(any(feature = "sql", feature = "wire"))]
pub(crate) use shared::SharedFile;

/// An engine as its store sees it: something to close at the store's close.
pub trait Engine: Send + Sync + 'static {
    /// Closes the engine. A second call, or a call after the engine closed by
    /// itself, does nothing.
    fn close(&self) -> Result<()>;

    /// The store's clock was moved by hand, as a test moves it: what sleeps
    /// until a time reads the clock again.
    fn clock_moved(&self) {}

    /// Copies the engine's files into `into`, each at the path it has in the
    /// store's directory, as one moment of the engine while it keeps working.
    /// An engine whose file another lends copies nothing: the lender does.
    fn snapshot(&self, into: &Path) -> Result<Vec<Copied>> {
        let _ = into;
        Ok(Vec::new())
    }
}

/// A file of a snapshot: its path inside the store, `/` between its parts,
/// as `kv.db`, `sql/app.db` or `blobs/objects/1a3/1a3f07`, and the engine
/// whose it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Copied {
    pub name: String,
    pub engine: &'static str,
}

/// Copies an SQLite file of an engine's, `name` inside the store, into the
/// snapshot at `into`, its directory made.
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
    expect(dead_code, reason = "every engine copies its files with it, and a store of none has none")
)]
pub(crate) fn copy_file(file: &crate::sqlite::File, name: &str, engine: &'static str, into: &Path) -> Result<Copied> {
    let target = into.join(name);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|error| crate::Error::io(format!("snapshot {name}"), error))?;
    }
    file.copy_into(&target).map_err(|error| error.within(format!("snapshot {name}")))?;
    Ok(Copied { name: name.to_owned(), engine })
}

/// The store's calls for the engines opened against it.
pub trait Host {
    /// The engine of type `E` this store has open, or the one `open` makes and
    /// the store then closes at its own close; every handle shares it.
    fn engine<E: Engine>(&self, open: impl FnOnce(&crate::Store) -> Result<Arc<E>>) -> Result<Arc<E>>;

    /// Closes `engine` when the store closes, the last attached first.
    fn attach(&self, engine: Arc<dyn Engine>) -> Result<()>;

    /// Runs `work` every `period` on the store's background thread, one task at
    /// a time, and sooner when what it gives back wakes it. A failure is logged
    /// once and a recovery once. Nothing runs when the store was opened without
    /// background work.
    fn every(&self, name: &str, period: Duration, work: impl FnMut() -> Result<()> + Send + 'static) -> Result<Wake>;

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
