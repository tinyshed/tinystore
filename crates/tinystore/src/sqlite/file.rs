use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;

use super::connection::{self, Role, execute, sql_error};
use super::group::{BEGIN, COMMIT, Group, ROLLBACK, Work};
use super::migrate::{self, Migration};
use super::readers::Readers;
use super::{Config, Tx};
use crate::{Error, ErrorKind, Result};

/// One SQLite file: one writer whose commits are grouped, and a pool of
/// readers. Every engine's file is one of these.
pub(crate) struct File {
    path: PathBuf,
    writer: Mutex<Connection>,
    group: Group,
    readers: Readers,
    closed: AtomicBool,
}

impl File {
    pub(crate) fn open(path: impl Into<PathBuf>, config: Config) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| Error::io(path.display().to_string(), error))?;
        }
        let config = Arc::new(config);
        let writer = connection::open(&path, &config, Role::Writer)?;
        Ok(Self {
            group: Group::new(config.group),
            readers: Readers::new(path.clone(), Arc::clone(&config)),
            writer: Mutex::new(writer),
            path,
            closed: AtomicBool::new(false),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Runs `write` in a commit it may share with the writes queued beside it,
    /// and hands back its value once that commit is as durable as the file's
    /// configuration says.
    ///
    /// `write` runs on whichever caller leads the commit, so it owns what it
    /// writes; it is built before it waits, and no application code runs while
    /// the group does. `bytes` is what it adds to the commit.
    pub(crate) fn write<T, F>(&self, bytes: usize, write: F) -> Result<T>
    where
        F: FnOnce(&Tx<'_>) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        self.refuse_when_closed("a write")?;
        let slot = Arc::new(Mutex::new(None));
        let filled = Arc::clone(&slot);
        let work: Work = Box::new(move |tx| {
            let value = write(tx)?;
            *lock(&filled) = Some(value);
            Ok(())
        });
        self.group.write(&self.writer, bytes, work).map_err(|error| error.within(self.describe()))?;
        lock(&slot)
            .take()
            .ok_or_else(|| Error::internal(format!("{}: a write committed without its value", self.describe())))
    }

    /// Runs `work` in a transaction that holds the writer alone, for reads that
    /// decide what to write. It commits when `work` succeeds and rolls back
    /// when it fails or panics, and it pays its own sync.
    pub(crate) fn transaction<T>(&self, work: impl FnOnce(&Tx<'_>) -> Result<T>) -> Result<T> {
        self.refuse_when_closed("a transaction")?;
        let connection = lock(&self.writer);
        execute(&connection, BEGIN).map_err(|error| sql_error(format!("{}: a transaction", self.describe()), error))?;
        let mut open = Open { connection: &connection, finished: false };
        let value = work(&Tx::new(&connection))?;
        open.finished = true;
        execute(&connection, COMMIT).map_err(|error| {
            if !connection.is_autocommit() {
                let _ = execute(&connection, ROLLBACK);
            }
            Error::new(ErrorKind::OutcomeUnknown, format!("{}: a transaction's commit", self.describe()))
                .with_source(error)
        })?;
        Ok(value)
    }

    /// Runs `read` on a reader, in one snapshot of the file.
    pub(crate) fn read<T>(&self, read: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.refuse_when_closed("a read")?;
        self.readers.read(read)
    }

    pub(crate) fn migrate(&self, history: &str, migrations: &[Migration]) -> Result<()> {
        self.transaction(|tx| migrate::apply(tx, history, migrations)).map_err(|error| error.within(self.describe()))
    }

    /// Closes the readers left unused for too long; the store runs it now and
    /// then.
    pub(crate) fn sweep(&self) -> usize {
        self.readers.sweep()
    }

    pub(crate) fn readers_open(&self) -> usize {
        self.readers.open()
    }

    pub(crate) fn commits(&self) -> u64 {
        self.group.commits()
    }

    /// Refuses new work, answers the writes still queued, closes the readers
    /// and truncates the WAL. A commit under way finishes first.
    pub(crate) fn close(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.group.close();
        self.readers.close();
        let connection = lock(&self.writer);
        // A truncated WAL leaves the directory as small as its data.
        connection
            .query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()))
            .map_err(|error| sql_error(format!("{}: its last checkpoint", self.describe()), error))
    }

    fn refuse_when_closed(&self, what: &str) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(Error::closed(format!("{}: {what}", self.describe())));
        }
        Ok(())
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

/// A transaction of its own, rolled back unless it finished.
struct Open<'a> {
    connection: &'a Connection,
    finished: bool,
}

impl Drop for Open<'_> {
    fn drop(&mut self) {
        if !self.finished && !self.connection.is_autocommit() {
            let _ = execute(self.connection, ROLLBACK);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "file_tests.rs"]
mod tests;
