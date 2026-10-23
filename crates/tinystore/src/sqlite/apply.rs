//! A copy's writer: each commit of its home applied whole, page for page,
//! through SQLite's `sqlite_dbpage`, so that readers of the copy see whole
//! commits and only SQLite's pager writes the file.
//!
//! The pages go in one transaction, page 1 last. A statement checks the
//! schema's cookie, which page 1 holds, before it runs: were page 1 written
//! first, the next statement would read a schema whose tables' pages are not
//! all written yet. A truncation is a row of its own, `(n, NULL)`, which
//! drops page `n` and those after it, and only the last row may be one, since
//! a page written after it cancels it.
//!
//! The applier runs the WAL's checkpoints itself, as a home's capture does:
//! each commit says where the WAL ends, which the copy keeps with its
//! position, and a checkpoint runs only once the copy synced that position.
//! A WAL starts over only after a checkpoint, so what it holds past the
//! position kept is exactly what a crash left unnamed.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;

use super::capture::{self, Position};
use super::connection::{self, Role, execute, sql_error};
use super::group::{BEGIN, ROLLBACK};
use super::{Config, Durability};
use crate::{Error, ErrorKind, Result};

const WRITE_PAGE: &str = "insert into sqlite_dbpage (pgno, data) values (?1, ?2)";
const WRITE_FIRST: &str = "insert into sqlite_dbpage (pgno, data) values (1, ?1)";
const WRITE_FIRST_AND_CUT: &str = "insert into sqlite_dbpage (pgno, data) values (1, ?1), (?2, null)";
const PAGES: &str = "pragma page_count";
const CHECKPOINT: &str = super::given::own!("pragma wal_checkpoint(passive)");
const PAGE_SIZE: &str = "pragma page_size";

/// The connection that writes a copy, opened at its first commit, whose
/// pages say what size the file's pages are.
pub(crate) struct Applier {
    path: PathBuf,
    durability: Durability,
    connection: Mutex<Option<Connection>>,
    wal: Mutex<Wal>,
}

/// Where the WAL stood after the applier's last commit.
#[derive(Default)]
struct Wal {
    at: Position,
    /// The frame the last checkpoint reached in this generation.
    checkpointed: u32,
    /// A checkpoint ran since: the next commit may begin a generation.
    restarted: bool,
}

impl Applier {
    pub(crate) fn new(path: impl Into<PathBuf>, durability: Durability) -> Applier {
        Applier { path: path.into(), durability, connection: Mutex::new(None), wal: Mutex::default() }
    }

    /// Writes `pages` of `page_size` bytes and leaves the file `size` pages,
    /// in one transaction; where the WAL ends after it.
    pub(crate) fn apply(&self, page_size: u32, size: u32, pages: &[(u32, &[u8])]) -> Result<Position> {
        let mut held = self.lock();
        let connection = self.connection(&mut held, page_size)?;
        capture::take_wal_end();
        execute(connection, BEGIN).map_err(|error| sql_error("a copy's commit: its transaction", error))?;
        let written = write(connection, page_size, size, pages);
        let committed =
            written.and_then(|()| connection::commit(connection).map_err(|error| sql_error("a copy's commit", error)));
        if committed.is_err() && !connection.is_autocommit() {
            let _ = execute(connection, ROLLBACK);
        }
        // Page 1 written through sqlite_dbpage leaves the connection's own
        // schema and file size where they were: it reads the next commit as
        // over a file that is not there. A new connection reads it as it is.
        if pages.iter().any(|&(number, _)| number == 1) || committed.is_err() {
            held.take();
        }
        committed.map_err(|error| error.within(self.path.display()))?;
        self.ended(capture::take_wal_end()).map_err(|error| error.within(self.path.display()))
    }

    /// Where the WAL ends after the commit that ended at `frame`.
    fn ended(&self, frame: Option<u32>) -> Result<Position> {
        let mut wal = self.wal.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(frame) = frame else {
            return Ok(wal.at);
        };
        let follows = !wal.restarted && wal.at != Position::default() && frame > wal.at.frame;
        let at = if follows { Position { frame, ..wal.at } } else { capture::wal_at(&self.path, frame)? };
        if !at.same_generation(&wal.at) {
            wal.checkpointed = 0;
        }
        (wal.at, wal.restarted) = (at, false);
        Ok(at)
    }

    /// Whether the WAL grew by as many frames as SQLite's own checkpoint waits
    /// for since the last one.
    pub(crate) fn is_long(&self) -> bool {
        let wal = self.wal.lock().unwrap_or_else(PoisonError::into_inner);
        wal.at.frame >= wal.checkpointed + super::capture::CHECKPOINT_AT
    }

    /// Checkpoints the WAL, once the copy synced the position that names
    /// where it ends.
    pub(crate) fn checkpoint(&self) -> Result<()> {
        let held = self.lock();
        let Some(connection) = held.as_ref() else {
            return Ok(());
        };
        connection
            .query_row(CHECKPOINT, [], |_| Ok(()))
            .map_err(|error| sql_error(self.path.display().to_string(), error))?;
        let mut wal = self.wal.lock().unwrap_or_else(PoisonError::into_inner);
        (wal.checkpointed, wal.restarted) = (wal.at.frame, true);
        Ok(())
    }

    /// Closes the connection, so that the file can be closed or removed.
    pub(crate) fn close(&self) {
        self.lock().take();
    }

    fn connection<'a>(&self, held: &'a mut Option<Connection>, page_size: u32) -> Result<&'a Connection> {
        if held.is_none() {
            let config = Config { page_size, durability: self.durability, keep_wal: true, ..Config::default() };
            let connection = open(&self.path, &config)?;
            capture::hook_wal(&connection);
            *held = Some(connection);
        }
        Ok(held.as_ref().expect("opened just above"))
    }

    fn lock(&self) -> MutexGuard<'_, Option<Connection>> {
        self.connection.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Copies every commit the WAL beside `path` holds into the file and empties
/// it: a copy's, each named, as the copy becomes a home whose capture would
/// take what its WAL holds for commits of its own.
pub(crate) fn checkpoint_away(path: &Path) -> Result<()> {
    let wal = super::capture::wal_of(path);
    if !wal.exists() {
        return Ok(());
    }
    let config = Config { keep_wal: true, ..Config::default() };
    let connection = connection::open(path, &config, Role::Writer)?;
    let busy: i64 = connection
        .query_row(super::file::TRUNCATE, [], |row| row.get(0))
        .map_err(|error| sql_error(path.display().to_string(), error))?;
    if busy != 0 {
        return Err(Error::new(
            ErrorKind::Unavailable,
            format!("{}: a reader holds its WAL, which a copy that becomes a home empties first", path.display()),
        ));
    }
    Ok(())
}

fn open(path: &Path, config: &Config) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| Error::io(path.display().to_string(), error))?;
    }
    let connection = connection::open(path, config, Role::Writer)?;
    super::pages::refuse_without_dbpage(&connection, path)?;
    Ok(connection)
}

fn write(connection: &Connection, page_size: u32, size: u32, pages: &[(u32, &[u8])]) -> Result<()> {
    let own_size: u32 = connection.query_row(PAGE_SIZE, [], |row| row.get(0)).map_err(|e| sql_error("its pages", e))?;
    if own_size != page_size {
        return Err(Error::new(
            ErrorKind::Corrupt,
            format!("its pages are {own_size} bytes and its home's {page_size}: it is a copy of another file"),
        ));
    }
    let had: u32 = connection.query_row(PAGES, [], |row| row.get(0)).map_err(|e| sql_error("its pages", e))?;
    let mut first = None;
    let mut statement = connection.prepare_cached(WRITE_PAGE).map_err(|error| sql_error("a page", error))?;
    for &(number, page) in pages {
        if number == 1 {
            first = Some(page);
            continue;
        }
        statement
            .execute(rusqlite::params![number, page])
            .map_err(|error| sql_error(format!("page {number}"), error))?;
    }
    drop(statement);
    let cut = had.max(1) > size;
    match (first, cut) {
        (Some(first), true) => connection.execute(WRITE_FIRST_AND_CUT, rusqlite::params![first, size + 1]),
        (Some(first), false) => connection.execute(WRITE_FIRST, [first]),
        (None, false) => Ok(0),
        (None, true) => {
            return Err(Error::new(ErrorKind::Corrupt, "a commit that makes the file smaller writes no page 1"));
        }
    }
    .map(drop)
    .map_err(|error| sql_error("page 1", error))
}
