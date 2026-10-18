//! A file one engine opened that others keep their tables in as well, and
//! where a handle opens.

use std::fmt;
use std::sync::{Arc, Mutex};

use super::Engine;
use crate::sqlite::File;
use crate::store::WeakStore;
use crate::{Error, Result, Store};

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
    #[cfg_attr(
        not(any(feature = "kv", feature = "jobs", feature = "search", feature = "replication")),
        expect(dead_code, reason = "only kv's, jobs' and search's handles are guests")
    )]
    store: WeakStore,
    #[cfg_attr(
        not(any(feature = "kv", feature = "jobs", feature = "search")),
        expect(dead_code, reason = "only kv's, jobs' and search's handles are guests")
    )]
    guests: Mutex<crate::store::Engines>,
}

impl SharedFile {
    #[cfg(feature = "sql")]
    pub(crate) fn new(file: File, owner: String, store: &Store) -> SharedFile {
        SharedFile { file: Arc::new(file), owner, store: store.downgrade(), guests: Mutex::default() }
    }

    /// The store the file is open in, while it is open.
    #[cfg(all(feature = "replication", feature = "sql"))]
    pub(crate) fn store(&self) -> Result<Store> {
        self.store.upgrade().ok_or_else(|| Error::closed(format!("{}: the store closed", self.owner)))
    }

    pub(crate) fn file(&self) -> &Arc<File> {
        &self.file
    }

    #[cfg_attr(
        not(any(feature = "kv", feature = "jobs", feature = "search")),
        expect(dead_code, reason = "only kv's, jobs' and search's handles are guests")
    )]
    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    /// The engine of type `E` kept in this file, or the one `open` makes; every
    /// handle opened from the file shares it.
    #[cfg_attr(
        not(any(feature = "kv", feature = "jobs", feature = "search")),
        expect(dead_code, reason = "only kv's, jobs' and search's handles are guests")
    )]
    pub(crate) fn engine<E: Engine>(&self, open: impl FnOnce(&Store) -> Result<Arc<E>>) -> Result<Arc<E>> {
        let store = self.store.upgrade().ok_or_else(|| Error::closed(format!("{}: the store closed", self.owner)))?;
        store.kept_or_opened(&self.guests, open)
    }
}

impl fmt::Debug for SharedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SharedFile({})", self.owner)
    }
}

/// Where a handle opens: in its engine's own file of the store, `kv.db` or
/// `jobs.db`, or in a file another engine lends, as a database does.
#[cfg(any(feature = "kv", feature = "jobs"))]
#[derive(Clone, Debug)]
pub(crate) enum Home {
    Store(Store),
    #[cfg_attr(
        not(any(feature = "sql", feature = "wire")),
        expect(dead_code, reason = "only a database lends its file, and the wire opens a handle in one")
    )]
    Shared(Arc<SharedFile>),
}
