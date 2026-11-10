//! A file one engine opened that others keep their tables in as well, and
//! where a handle opens.

use std::fmt;
use std::sync::{Arc, Mutex};

use super::Engine;
use crate::sqlite::File;
use crate::store::WeakStore;
use crate::{Error, Result, Store};

#[cfg(feature = "wire")]
const COUNT_TABLES: &str = "select count(*) from sqlite_schema where type = 'table' and name = ?1";

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
        not(any(
            feature = "kv",
            feature = "jobs",
            feature = "blobs",
            feature = "search",
            feature = "records",
            feature = "replication",
            feature = "async"
        )),
        expect(
            dead_code,
            reason = "read by kv's, jobs', blobs', search's and records' guests, a copy's writes and the async API's databases"
        )
    )]
    store: WeakStore,
    #[cfg_attr(
        not(any(feature = "kv", feature = "jobs", feature = "blobs", feature = "search", feature = "records")),
        expect(dead_code, reason = "only kv's, jobs', blobs', search's and records' handles are guests")
    )]
    guests: Mutex<crate::store::Engines>,
}

impl SharedFile {
    #[cfg(feature = "sql")]
    pub(crate) fn new(file: File, owner: String, store: &Store) -> SharedFile {
        SharedFile { file: Arc::new(file), owner, store: store.downgrade(), guests: Mutex::default() }
    }

    /// The store the file is open in, while it is open.
    #[cfg(all(feature = "sql", any(feature = "records", feature = "replication", feature = "async")))]
    pub(crate) fn store(&self) -> Result<Store> {
        self.store.upgrade().ok_or_else(|| Error::closed(format!("{}: the store closed", self.owner)))
    }

    pub(crate) fn file(&self) -> &Arc<File> {
        &self.file
    }

    #[cfg_attr(
        not(any(feature = "kv", feature = "jobs", feature = "blobs", feature = "search", feature = "records")),
        expect(dead_code, reason = "only kv's, jobs', blobs', search's and records' handles are guests")
    )]
    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    /// Refuses a file without the table `table`, as a name it does not hold:
    /// the engine that keeps `what` there never made its tables, which an
    /// open by a connection that only reads would.
    #[cfg(feature = "wire")]
    pub(crate) fn keeps(&self, table: &str, what: &str) -> Result<()> {
        let tables: i64 = self.file.read(|connection| {
            connection
                .prepare_cached(COUNT_TABLES)
                .and_then(|mut count| count.query_row([table], |row| row.get::<_, i64>(0)))
                .map_err(|error| crate::sqlite::sql_error(format!("{}: its tables", self.owner), error))
        })?;
        if tables == 0 {
            return Err(super::not_held(what));
        }
        Ok(())
    }

    /// The engine of type `E` kept in this file, or the one `open` makes; every
    /// handle opened from the file shares it.
    #[cfg_attr(
        not(any(feature = "kv", feature = "jobs", feature = "blobs", feature = "search", feature = "records")),
        expect(dead_code, reason = "only kv's, jobs', blobs', search's and records' handles are guests")
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

/// A write of a handle kept in a file another engine lends, which the
/// lender's batch runs in its own savepoint: a queue's add in a database's
/// batch, which commits with the batch's rows and shares the batch's commit.
#[cfg(all(feature = "sql", any(feature = "jobs", feature = "records")))]
pub(crate) struct LentWrite {
    /// The file the handle is kept in, which must be the batch's.
    pub(crate) file: Arc<File>,
    /// The file as an error names it: `jobs.db`, or `sql app`.
    pub(crate) place: String,
    /// The handle and what it writes, as an error names them: `jobs queue
    /// indexing: id "n-7"`.
    pub(crate) describe: String,
    /// What the write adds to a shared commit.
    pub(crate) weight: usize,
    pub(crate) write: LentWork,
}

/// What a lent write runs on the writer, inside the batch's savepoint.
#[cfg(all(feature = "sql", any(feature = "jobs", feature = "records")))]
pub(crate) type LentWork = Box<dyn FnOnce(&crate::sqlite::Tx<'_>) -> Result<Wrote> + Send>;

#[cfg(all(feature = "sql", any(feature = "jobs", feature = "records")))]
impl fmt::Debug for LentWrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LentWrite({})", self.describe)
    }
}

/// What a lent write did: the jobs or events it changed, and what runs once
/// its commit is durable.
#[cfg(all(feature = "sql", any(feature = "jobs", feature = "records")))]
pub(crate) struct Wrote {
    pub(crate) changes: u64,
    /// Runs on the thread that committed, and waits for nothing: a queue's
    /// workers woken for the job it added.
    pub(crate) committed: Option<Box<dyn FnOnce() + Send>>,
    /// Runs before the batch answers, off the thread that committed, and may
    /// wait for another file: a stream's events moved into records.db.
    pub(crate) before_answer: Option<Box<dyn FnOnce() + Send>>,
}
