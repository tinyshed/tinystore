//! A directory's changes since a cursor: each file told once for what it is
//! now, as a copy kept in step needs it, not a log of every event. A row
//! carries the number of its last change, so a walk from the start needs no
//! snapshot beside it: a file that changes while the program walks is met
//! again later, at its new number.
//!
//! ```text
//! since the cursor:   added, then changed  → added
//!                     changed twice        → changed
//!                     added, then removed  → removed, which a program that never saw it ignores
//! ```

use std::sync::Arc;
use std::time::Duration;

use rusqlite::params;

use super::Directory;
use super::listed::Row;
use crate::blobs::engine::{Blobs, rows_of};
use crate::blobs::info::FileInfo;
use crate::blobs::paths::Folder;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, ErrorKind, Result};

/// Changes a page holds at most.
pub(crate) const PAGE_CHANGES: usize = 1000;
/// How long a removed file's line is kept, for a program that was off.
pub(crate) const REMOVED_KEPT: Duration = Duration::from_secs(30 * 24 * 60 * 60);

const SELECT_CHANGES: &str = "select path, size, modified, change, added, removed is not null
    from _tinystore_blobs_listed where directory = ?1 and change > ?2 order by change limit cast(?3 as integer)";
const SELECT_MARKS: &str = "select purged, changes from _tinystore_blobs_directories where id = ?1";
const SELECT_THERE: &str = "select path from _tinystore_blobs_listed
    where directory = ?1 and path > ?2 and removed is null order by path limit cast(?3 as integer)";
pub(super) const TAKE_CHANGES: &str =
    "update _tinystore_blobs_directories set changes = changes + ?2 where id = ?1 returning changes";
pub(super) const MARK_REMOVED: &str =
    "update _tinystore_blobs_listed set removed = ?3, change = ?4 where directory = ?1 and path = ?2";
const PURGE: &str = "delete from _tinystore_blobs_listed where id in (
    select id from _tinystore_blobs_listed where removed is not null and removed < ?1 limit cast(?2 as integer)
) returning directory, change";
const MOVE_PURGED: &str = "update _tinystore_blobs_directories set purged = max(purged, ?2) where id = ?1";

/// What became of a file since a cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Change {
    Added,
    Changed,
    Removed,
}

/// One file of a page of changes: its whole path in the directory, and what
/// it carries now, none once it is removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub change: Change,
    pub info: Option<FileInfo>,
}

/// A page of changes in the order the scans wrote them, and the cursor to ask
/// from next: with no items, the one to ask from after the next scan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub items: Vec<FileChange>,
    pub next: String,
}

/// A page of a directory's changes, its cursor before [`ChangesCall::page`].
#[derive(Debug)]
#[must_use = "changes read nothing until their page"]
pub struct ChangesCall<'d> {
    directory: &'d Directory,
    after: Option<String>,
}

impl Directory {
    /// The changes of every file since a cursor, or from the first file the
    /// directory ever listed, each `added`.
    pub fn changes(&self) -> ChangesCall<'_> {
        ChangesCall { directory: self, after: None }
    }
}

impl ChangesCall<'_> {
    /// The changes past this cursor, a page's `next`.
    pub fn after(mut self, cursor: impl Into<String>) -> Self {
        self.after = Some(cursor.into());
        self
    }

    pub fn page(self) -> Result<Changes> {
        let directory = self.directory;
        directory.whole_directory("changes")?;
        page(&directory.blobs, directory.opened.id, self.after.as_deref()).map_err(|error| directory.fail("", error))
    }
}

/// A row of the list as a page of changes reads it.
struct Line {
    row: Row,
    change: i64,
    added: i64,
    removed: bool,
}

fn page(blobs: &Arc<Blobs>, directory: i64, after: Option<&str>) -> Result<Changes> {
    let cursor = match after {
        None => 0,
        Some(text) => text.parse::<i64>().ok().filter(|&cursor| cursor >= 0).ok_or_else(|| {
            Error::invalid(format!("a cursor {text:?}: a page of changes' next is the only one there is"))
        })?,
    };
    let limit = i64::try_from(PAGE_CHANGES).unwrap_or(i64::MAX);
    let ((purged, last), lines) = blobs.file.read(|connection| {
        let marks: (i64, i64) = connection
            .prepare_cached(SELECT_MARKS)
            .and_then(|mut select| select.query_row([directory], |row| Ok((row.get(0)?, row.get(1)?))))
            .map_err(|error| sql_error("blobs: a directory's marks", error))?;
        let changes = (SELECT_CHANGES, "blobs: a directory's changes");
        let lines: Vec<Line> = rows_of(connection, changes, params![directory, cursor, limit], |row| {
            Ok(Line { row: Row::read(row)?, change: row.get(3)?, added: row.get(4)?, removed: row.get(5)? })
        })?;
        Ok((marks, lines))
    })?;
    if after.is_some() && cursor < purged {
        let why = "a cursor older than the 30 days a removed file's line is kept: read the changes without one";
        return Err(Error::new(ErrorKind::TooOld, why));
    }
    // A page short of full read to the last change there is, and its next is
    // that change, past every removed line upkeep deleted before it. The marks
    // are read before the lines, so a scan that commits between them makes a
    // later line the newest, never one the page did not read.
    let newest = lines.last().map_or(cursor, |line| line.change);
    let next = if lines.len() < PAGE_CHANGES { newest.max(last) } else { newest }.to_string();
    let items = lines.into_iter().map(|line| told(line, cursor)).collect();
    Ok(Changes { items, next })
}

/// What a line tells a reader at `cursor`: added when it came after it.
fn told(line: Line, cursor: i64) -> FileChange {
    let change = match (line.removed, line.added > cursor) {
        (true, _) => Change::Removed,
        (false, true) => Change::Added,
        (false, false) => Change::Changed,
    };
    let info = (change != Change::Removed).then(|| line.row.info(&Folder::default()));
    FileChange { path: line.row.path, change, info }
}

/// Tells every file of the list removed, a thousand lines a commit.
pub(super) fn forget(blobs: &Arc<Blobs>, directory: i64) -> Result<()> {
    let mut after = String::new();
    loop {
        let limit = i64::try_from(PAGE_CHANGES).unwrap_or(i64::MAX);
        let there = (SELECT_THERE, "blobs: a directory's files");
        let paths: Vec<String> = blobs
            .file
            .read(|connection| rows_of(connection, there, params![directory, after, limit], |row| row.get(0)))?;
        let Some(last) = paths.last() else { return Ok(()) };
        after.clone_from(last);
        let full = paths.len() == PAGE_CHANGES;
        let now = blobs.now();
        blobs.file.write(0, move |tx| remove_lines(tx, directory, &paths, now))?;
        if !full {
            return Ok(());
        }
    }
}

/// Marks the lines of `paths` removed at `now`, each with a change of its own.
pub(super) fn remove_lines(tx: &Tx<'_>, directory: i64, paths: &[String], now: i64) -> Result<()> {
    let failed = |error| sql_error("blobs: a directory's removed files", error);
    let last = take_changes(tx, directory, paths.len())?;
    let mut mark = tx.prepare_cached(MARK_REMOVED).map_err(failed)?;
    let first = last - i64::try_from(paths.len()).unwrap_or(0);
    for (path, change) in paths.iter().zip(first + 1..) {
        mark.execute(params![directory, path, now, change]).map_err(failed)?;
    }
    Ok(())
}

/// Takes `count` change numbers of a directory, and says the last.
pub(super) fn take_changes(tx: &Tx<'_>, directory: i64, count: usize) -> Result<i64> {
    let count = i64::try_from(count).unwrap_or(i64::MAX);
    tx.prepare_cached(TAKE_CHANGES)
        .and_then(|mut take| take.query_row(params![directory, count], |row| row.get(0)))
        .map_err(|error| sql_error("blobs: a directory's changes", error))
}

impl Blobs {
    /// Deletes the lines of files removed more than 30 days ago, a batch a
    /// transaction, and moves each directory's purged mark past them, so that
    /// a cursor before them is too old rather than miss a removal.
    pub(crate) fn purge_removed(self: &Arc<Self>) -> Result<usize> {
        if self.kept.lent_by.is_some() {
            return Ok(0);
        }
        let before = self.now().saturating_sub(crate::clock::millis(REMOVED_KEPT));
        crate::engine::in_batches(|| {
            self.file.write(0, move |tx| {
                let failed = |error| sql_error("blobs: purge a directory's removed lines", error);
                let batch = i64::try_from(crate::engine::BATCH).unwrap_or(i64::MAX);
                let purge = (PURGE, "blobs: purge a directory's removed lines");
                let purged: Vec<(i64, i64)> =
                    rows_of(tx, purge, params![before, batch], |row| Ok((row.get(0)?, row.get(1)?)))?;
                let mut newest: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
                for &(directory, change) in &purged {
                    let entry = newest.entry(directory).or_insert(change);
                    *entry = (*entry).max(change);
                }
                for (directory, change) in newest {
                    tx.execute(MOVE_PURGED, params![directory, change]).map_err(failed)?;
                }
                Ok(purged.len())
            })
        })
    }
}
