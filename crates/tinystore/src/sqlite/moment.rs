//! A store's SQLite files copied at one moment of them all, so that a row of
//! one file that names what another holds is copied with what it names, or
//! neither is. The commits of every file stop for the instant a read of each
//! begins, and each file is then copied from its read while they go on.
//!
//! ```text
//! shut the gate       a commit waits before it takes its writer, a transaction under way goes on
//! hold every writer   none waited for while another is held
//! begin every read    the moment
//! open the gate       the commits go on, and each file is copied from its read
//! ```

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};

use super::Config;
use super::connection::{self, Role, execute, sql_error};
use super::given::own;
use crate::sync::lock;
use crate::{Error, Result};

const BEGIN: &str = own!("begin");
const END: &str = own!("commit");
const PAGES: &str = "pragma page_count";

/// What holds a store's files still for an instant. While a snapshot keeps it
/// shut, a commit waits before it takes its file's writer, unless its thread
/// holds a writer in a transaction: that transaction may wait for another
/// file's writer, and the snapshot waits for it to end instead.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    /// The snapshots that keep it shut.
    shut: AtomicUsize,
    waiting: Mutex<()>,
    opened: Condvar,
}

impl Gate {
    /// Waits while the gate is shut, unless this thread holds a writer.
    pub(crate) fn pass(&self) {
        if self.shut.load(Ordering::Acquire) == 0 || super::in_a_transaction() {
            return;
        }
        let mut waiting = lock(&self.waiting);
        while self.shut.load(Ordering::Acquire) > 0 {
            waiting = self.opened.wait(waiting).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn shut(&self) -> Shut<'_> {
        self.shut.fetch_add(1, Ordering::AcqRel);
        Shut(self)
    }
}

/// The gate kept shut, until it is dropped.
struct Shut<'a>(&'a Gate);

impl Drop for Shut<'_> {
    fn drop(&mut self) {
        let _waiting = lock(&self.0.waiting);
        self.0.shut.fetch_sub(1, Ordering::AcqRel);
        self.0.opened.notify_all();
    }
}

/// A file to copy at the moment, and the writer its commits take while an
/// engine has it open; none for a file no engine holds, which nothing writes,
/// and none for a copy, which its home writes.
pub(crate) struct Source {
    pub(super) path: PathBuf,
    pub(super) writer: Option<Arc<Mutex<Connection>>>,
}

impl Source {
    /// A file that lies in the store's directory with no engine holding it.
    pub(crate) fn unheld(path: PathBuf) -> Source {
        Source { path, writer: None }
    }
}

/// Copies each file of `files` to its target, which is not there yet, at one
/// moment of them all; `at_the_moment` runs at it too, every writer held, and
/// what it gives back is handed back once the files are copied.
pub(crate) fn copy_at_one_moment<T>(
    gate: &Gate,
    files: &[(Source, PathBuf)],
    at_the_moment: impl FnOnce() -> T,
) -> Result<T> {
    let reads = files.iter().map(|(source, _)| Read::open(&source.path)).collect::<Result<Vec<_>>>()?;
    let writers: Vec<&Mutex<Connection>> = files.iter().filter_map(|(source, _)| source.writer.as_deref()).collect();
    let kept = {
        let _shut = gate.shut();
        let _still = hold_still(&writers);
        reads.iter().try_for_each(Read::begin)?;
        at_the_moment()
    };
    for (read, (_, target)) in reads.iter().zip(files) {
        read.copy_to(target)?;
    }
    Ok(kept)
}

/// Holds every writer, waiting for none while it holds another: a
/// transaction that holds one writer may wait for another's, which a wait
/// holding that one would make it wait for ever. The gate is shut, so only
/// transactions under way take a writer meanwhile, and the loop ends once
/// they have.
fn hold_still<'a>(writers: &[&'a Mutex<Connection>]) -> Vec<MutexGuard<'a, Connection>> {
    let mut waited_for: Option<usize> = None;
    loop {
        // the one the last round found busy is waited for first, holding nothing
        let mut held: Vec<MutexGuard<'a, Connection>> = waited_for.map(|at| lock(writers[at])).into_iter().collect();
        let mut busy = None;
        for (at, writer) in writers.iter().enumerate().filter(|&(at, _)| Some(at) != waited_for) {
            match writer.try_lock() {
                Ok(guard) => held.push(guard),
                Err(TryLockError::Poisoned(poisoned)) => held.push(poisoned.into_inner()),
                Err(TryLockError::WouldBlock) => {
                    busy = Some(at);
                    break;
                }
            }
        }
        let Some(at) = busy else { return held };
        drop(held);
        waited_for = Some(at);
    }
}

/// A connection of the snapshot's own on a file, whose read, begun at the
/// moment, is what its copy holds.
struct Read {
    path: PathBuf,
    connection: Connection,
}

impl Read {
    fn open(path: &Path) -> Result<Read> {
        // A file no engine holds may be a node's, whose WAL holds what its log
        // may lack: the last connection's close must not checkpoint it.
        let config = Config { keep_wal: true, ..Config::default() };
        let connection = connection::open(path, &config, Role::Reader)?;
        Ok(Read { path: path.to_owned(), connection })
    }

    /// Begins the read and pins its snapshot with a first read of the file.
    fn begin(&self) -> Result<()> {
        let what = || format!("{}: its snapshot", self.path.display());
        execute(&self.connection, BEGIN).map_err(|error| sql_error(what(), error))?;
        let pinned = self.connection.query_row(PAGES, [], |row| row.get::<_, i64>(0));
        pinned.map(drop).map_err(|error| sql_error(what(), error))
    }

    /// Copies the read's pages beside `target`, then gives the copy its name
    /// as a file of one piece with no free page, as `vacuum into` writes one.
    fn copy_to(&self, target: &Path) -> Result<()> {
        let what = || format!("copy {} to {}", self.path.display(), target.display());
        let mut pages_name = target.as_os_str().to_owned();
        pages_name.push(".pages");
        let pages = PathBuf::from(pages_name);
        let copied = self.pages_into(&pages);
        let ended = execute(&self.connection, END).map_err(|error| sql_error(what(), error));
        let settled = copied.and(ended).and_then(|()| settle(&pages, target));
        let _ = std::fs::remove_file(&pages);
        settled.map_err(|error| error.within(what()))
    }

    /// Every page of the read, through SQLite's backup, which reads in the
    /// transaction its source has open rather than one of its own.
    fn pages_into(&self, pages: &Path) -> Result<()> {
        let failed = |error| sql_error(format!("copy the pages into {}", pages.display()), error);
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_EXRESCODE;
        let mut copy = Connection::open_with_flags(pages, flags).map_err(failed)?;
        let backup = Backup::new(&self.connection, &mut copy).map_err(failed)?;
        match backup.step(-1).map_err(failed)? {
            StepResult::Done => Ok(()),
            // -1 copies every page in one step, into a file nothing else opens
            other => Err(Error::internal(format!("the copy of the pages stopped short: {other:?}"))),
        }
    }
}

/// Makes the pages a file of one piece at `target`. Page 1 says WAL, as the
/// file read had it: a first read opens the copy's WAL, and the journal mode
/// set back writes it a file of one piece. Its free pages go where the file
/// gives them back; where it cannot, `vacuum into` writes it again without.
fn settle(pages: &Path, target: &Path) -> Result<()> {
    let failed = |error: rusqlite::Error| sql_error(format!("settle {}", pages.display()), error);
    let connection = Connection::open(pages).map_err(failed)?;
    connection.query_row("select count(*) from sqlite_schema", [], |row| row.get::<_, i64>(0)).map_err(failed)?;
    connection.pragma_update(None, "journal_mode", "delete").map_err(failed)?;
    connection.execute_batch("pragma incremental_vacuum").map_err(failed)?;
    let free: i64 = connection.query_row("pragma freelist_count", [], |row| row.get(0)).map_err(failed)?;
    if free > 0 {
        let into =
            target.to_str().ok_or_else(|| Error::invalid(format!("{}: a path that is no UTF-8", target.display())))?;
        connection.execute("vacuum into ?1", [into]).map_err(failed)?;
        return Ok(());
    }
    drop(connection);
    std::fs::rename(pages, target).map_err(|error| Error::io(format!("settle {}", target.display()), error))
}

#[cfg(test)]
#[path = "moment_tests.rs"]
mod tests;
