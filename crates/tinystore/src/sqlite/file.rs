use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;

use super::connection::{self, Role, execute, sql_error};
use super::group::{BEGIN, COMMIT, Done, Group, ROLLBACK, Work};
use super::migrate::{self, Migration};
use super::readers::Readers;
use super::{Config, Tx};
use crate::{Error, ErrorKind, Result};

const FOREIGN_KEYS_OFF: &str = "pragma foreign_keys = off";
const FOREIGN_KEYS_ON: &str = "pragma foreign_keys = on";

thread_local! {
    /// The files whose writer this thread holds in a transaction, by address.
    static HELD: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

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
        self.refuse_inside_a_transaction("a write", "would wait for the writer the transaction holds")?;
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

    /// `write` without waiting: it queues the write and returns, and `done` is
    /// called with the value, or the error, on the thread that commits it. For
    /// hosts whose one thread holds many writes in flight; `done` must not
    /// block, since the commits after it wait for it.
    pub(crate) fn submit<T, F, D>(&self, bytes: usize, write: F, done: D)
    where
        F: FnOnce(&Tx<'_>) -> Result<T> + Send + 'static,
        T: Send + 'static,
        D: FnOnce(Result<T>) + Send + 'static,
    {
        let refused = self.refuse_when_closed("a write").and_then(|()| {
            self.refuse_inside_a_transaction("a write", "would wait for the writer the transaction holds")
        });
        if let Err(error) = refused {
            return done(Err(error));
        }
        let slot = Arc::new(Mutex::new(None));
        let filled = Arc::clone(&slot);
        let work: Work = Box::new(move |tx| {
            let value = write(tx)?;
            *lock(&filled) = Some(value);
            Ok(())
        });
        let describe = self.describe();
        let answered: Done = Box::new(move |answer| {
            let value = answer.map_err(|error| error.within(&describe)).and_then(|()| {
                let missing = || Error::internal(format!("{describe}: a write committed without its value"));
                lock(&slot).take().ok_or_else(missing)
            });
            done(value);
        });
        self.group.submit(&self.writer, bytes, work, answered);
    }

    /// Runs `work` in a transaction that holds the writer alone, for reads that
    /// decide what to write. It commits when `work` succeeds and rolls back
    /// when it fails or panics, and it pays its own sync.
    ///
    /// A write to this file from inside `work`, other than through its `Tx`,
    /// would wait for the writer `work` holds: it fails `Invalid` instead.
    pub(crate) fn transaction<T, E: From<Error>>(&self, work: impl FnOnce(&Tx<'_>) -> Result<T, E>) -> Result<T, E> {
        self.refuse_when_closed("a transaction")?;
        self.refuse_inside_a_transaction("a transaction", "would wait for the writer the transaction holds")?;
        let connection = lock(&self.writer);
        self.transaction_on(&connection, work)
    }

    fn transaction_on<T, E: From<Error>>(
        &self,
        connection: &Connection,
        work: impl FnOnce(&Tx<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        execute(connection, BEGIN).map_err(|error| sql_error(format!("{}: a transaction", self.describe()), error))?;
        let _held = Held::enter(self.address());
        let mut open = Open { connection, finished: false };
        let value = work(&Tx::new(connection))?;
        open.finished = true;
        execute(connection, COMMIT).map_err(|error| {
            if !connection.is_autocommit() {
                let _ = execute(connection, ROLLBACK);
            }
            Error::new(ErrorKind::OutcomeUnknown, format!("{}: a transaction's commit", self.describe()))
                .with_source(error)
        })?;
        Ok(value)
    }

    /// Whether this thread holds the file's writer in a transaction.
    pub(crate) fn held_here(&self) -> bool {
        HELD.with(|held| held.borrow().contains(&self.address()))
    }

    /// Runs `read` on a reader, in one snapshot of the file. A read from inside
    /// a transaction of the file would not see what the transaction wrote: it
    /// fails `Invalid` instead.
    pub(crate) fn read<T>(&self, read: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.refuse_when_closed("a read")?;
        self.refuse_inside_a_transaction("a read", "would not see what the transaction wrote")?;
        self.readers.read(read)
    }

    pub(crate) fn migrate(&self, history: &str, migrations: &[Migration]) -> Result<()> {
        self.transaction(|tx| migrate::apply(tx, history, migrations)).map_err(|error| error.within(self.describe()))
    }

    /// Applies migrations as `migrate` does, with foreign keys off while they
    /// run and checked before the commit, as SQLite's procedure for changing a
    /// table asks: with them on, rebuilding a parent table deletes its
    /// children through `on delete cascade`, and nothing says so.
    pub(crate) fn migrate_checked(&self, history: &str, migrations: &[Migration]) -> Result<()> {
        self.refuse_when_closed("a migration")?;
        self.refuse_inside_a_transaction("a migration", "would wait for the writer the transaction holds")?;
        let connection = lock(&self.writer);
        let what = || format!("{}: {history}: foreign keys", self.describe());
        // foreign_keys cannot change inside a transaction, so it changes around one.
        execute(&connection, FOREIGN_KEYS_OFF).map_err(|error| sql_error(what(), error))?;
        let applied = self.transaction_on(&connection, |tx| {
            migrate::apply(tx, history, migrations)?;
            migrate::check_foreign_keys(tx, history)
        });
        let restored = execute(&connection, FOREIGN_KEYS_ON).map_err(|error| sql_error(what(), error));
        applied.map_err(|error| error.within(self.describe())).and(restored)
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

    fn refuse_inside_a_transaction(&self, what: &str, why: &str) -> Result<()> {
        if !self.held_here() {
            return Ok(());
        }
        Err(Error::invalid(format!(
            "{}: {what} from inside a transaction of the file {why}: make it through the transaction",
            self.describe()
        )))
    }

    fn address(&self) -> usize {
        self as *const File as usize
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

/// A file's writer this thread holds, from its transaction's start to its end.
struct Held(usize);

impl Held {
    fn enter(file: usize) -> Held {
        HELD.with(|held| held.borrow_mut().push(file));
        Held(file)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        HELD.with(|held| {
            let mut held = held.borrow_mut();
            if let Some(at) = held.iter().rposition(|&file| file == self.0) {
                held.remove(at);
            }
        });
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
