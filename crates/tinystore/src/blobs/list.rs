//! A folder's files: a page of them, every page, what they count, and a clear
//! of them all.

use std::sync::{Arc, LazyLock};

use rusqlite::params;

use super::change::{Change, hidden};
use super::files::Files;
use super::info::{FileInfo, FileRow, Page, Usage, file_columns};
use super::paths::MAX_PATH;
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// Files a page holds at most, and when a list does not say.
pub const PAGE_FILES: usize = 1000;

/// Files a clear deletes in its own transaction; past them it marks the
/// folder and maintenance deletes them.
pub(crate) const CLEAR_AT_ONCE: usize = 10_000;

const SELECT_PAGE: &str = concat!(
    "select ",
    file_columns!(),
    " from files as f where f.set_id = ?1 and f.path >= ?2 and f.path < ?3
        and (f.expires is null or f.expires > ?4) and not ",
    hidden!(),
    " order by f.path limit cast(?5 as integer)"
);
const SELECT_USAGE: &str = concat!(
    "select count(*), coalesce(sum(f.size), 0) from files as f where f.set_id = ?1 and f.path >= ?2
        and f.path < ?3 and (f.expires is null or f.expires > ?4) and not ",
    hidden!()
);
const COUNT_UNDER: &str = "select count(*) from (
    select 1 from files where set_id = ?1 and path >= ?2 and path < ?3 limit cast(?4 as integer))";
const DELETE_UNDER: &str = "delete from files where set_id = ?1 and path >= ?2 and path < ?3 returning content";
const MARK_CLEARED: &str = "insert into cleared (set_id, prefix, revision) values (?1, ?2, ?3)
    on conflict (set_id, prefix) do update set revision = excluded.revision";

/// Past every path: longer than a path may be, of the last character.
pub(crate) static PAST_EVERY: LazyLock<String> = LazyLock::new(|| "\u{10ffff}".repeat(MAX_PATH / 4 + 1));

/// A page of a folder's files, its options before [`List::page`].
#[derive(Debug)]
#[must_use = "a list reads nothing until its page"]
pub struct List<'f> {
    files: &'f Files,
    prefix: String,
    after: Option<String>,
    limit: usize,
}

impl Files {
    /// A page of this folder's files and of the folders in it, in the byte
    /// order of their paths: `10.jpg` before `9.jpg`.
    pub fn list(&self) -> List<'_> {
        List { files: self, prefix: String::new(), after: None, limit: PAGE_FILES }
    }

    /// Every file of this folder and of the folders in it, a page at a time,
    /// holding nothing between pages.
    pub fn all(&self) -> All {
        All { files: self.clone(), after: None, page: Vec::new().into_iter(), done: false }
    }

    /// The files of this folder and of the folders in it, and their bytes, as
    /// they are now: a count, not a bound.
    pub fn usage(&self) -> Result<Usage> {
        let folder = self.whole_folder()?;
        let (from, to) = self.folder.range("").map_err(|error| self.fail(&folder, error))?;
        let to = to.unwrap_or_else(|| PAST_EVERY.clone());
        let now = self.blobs.now();
        let (count, size): (i64, i64) = self
            .blobs
            .file
            .read(|connection| {
                connection
                    .prepare_cached(SELECT_USAGE)
                    .and_then(|mut statement| {
                        statement.query_row(params![self.set, from, to, now], |row| Ok((row.get(0)?, row.get(1)?)))
                    })
                    .map_err(|error| sql_error("blobs: a folder's usage", error))
            })
            .map_err(|error| self.fail(&folder, error))?;
        Ok(Usage { count: u64::try_from(count).unwrap_or(0), size: u64::try_from(size).unwrap_or(0) })
    }

    /// Removes this folder and every folder in it, at once for every reader,
    /// however many files it holds. On the files themselves it empties them.
    pub fn clear(&self) -> Result<()> {
        let folder = self.whole_folder()?;
        let blobs = Arc::clone(&self.blobs);
        let (set, prefix) = (self.set, self.folder.prefix().to_owned());
        let (from, to) = self.folder.range("").map_err(|error| self.fail(&folder, error))?;
        let to = to.unwrap_or_else(|| PAST_EVERY.clone());
        let bound = i64::try_from(CLEAR_AT_ONCE).unwrap_or(i64::MAX);
        let freed = self
            .blobs
            .file
            .write(0, move |tx| {
                let mut change = Change::new(tx, &blobs);
                let count: i64 = tx
                    .prepare_cached(COUNT_UNDER)
                    .and_then(|mut statement| statement.query_row(params![set, from, to, bound + 1], |row| row.get(0)))
                    .map_err(|error| sql_error("blobs: count a folder", error))?;
                if count <= bound {
                    let contents = change.deleted(DELETE_UNDER, params![set, from, to])?;
                    change.release_all(contents)?;
                } else {
                    let revision = change.next_revision()?;
                    change.execute(MARK_CLEARED, params![set, prefix, revision])?;
                }
                Ok(change.freed)
            })
            .map_err(|error| self.fail(&folder, error))?;
        self.blobs.let_files_go(&freed);
        Ok(())
    }

    /// The folder as an error names it, or why it is not one.
    fn whole_folder(&self) -> Result<String> {
        self.usable().map(|()| self.folder.prefix().to_owned())
    }

    fn page(&self, prefix: &str, after: Option<&str>, limit: usize) -> Result<Page> {
        if limit == 0 || limit > PAGE_FILES {
            return Err(Error::invalid(format!("a page of {limit} files; 1 to {PAGE_FILES}")));
        }
        let (mut from, to) = self.folder.range(prefix)?;
        if let Some(after) = after {
            let (past_after, _) = self.folder.range(after)?;
            from = from.max(format!("{past_after}\u{0}"));
        }
        let to = to.unwrap_or_else(|| PAST_EVERY.clone());
        let now = self.blobs.now();
        let wanted = i64::try_from(limit + 1).unwrap_or(i64::MAX);
        let rows = self.blobs.file.read(|connection| {
            let mut statement =
                connection.prepare_cached(SELECT_PAGE).map_err(|error| sql_error("blobs: a folder's page", error))?;
            let rows = statement
                .query_map(params![self.set, from, to, now, wanted], FileRow::read)
                .map_err(|error| sql_error("blobs: a folder's page", error))?;
            rows.collect::<rusqlite::Result<Vec<FileRow>>>().map_err(|error| sql_error("blobs: a folder's page", error))
        })?;
        let more = rows.len() > limit;
        let files =
            rows.into_iter().take(limit).map(|row| row.info(&self.folder)).collect::<Result<Vec<FileInfo>>>()?;
        let next = if more { files.last().map(|file| file.path.clone()) } else { None };
        Ok(Page { files, next })
    }
}

impl List<'_> {
    /// Only the files whose path within the folder starts with this text:
    /// `"photos/1"` finds `photos/10.jpg` too.
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// The page that starts past this path, as a page's `next` says it.
    pub fn after(mut self, path: impl Into<String>) -> Self {
        self.after = Some(path.into());
        self
    }

    pub fn limit(mut self, files: usize) -> Self {
        self.limit = files;
        self
    }

    pub fn page(self) -> Result<Page> {
        let folder = self.files.whole_folder()?;
        self.files
            .page(&self.prefix, self.after.as_deref(), self.limit)
            .map_err(|error| self.files.fail(&folder, error))
    }
}

/// Every file of a folder, a page at a time, as [`Files::all`] walks them.
#[derive(Debug)]
pub struct All {
    files: Files,
    after: Option<String>,
    page: std::vec::IntoIter<FileInfo>,
    done: bool,
}

impl Iterator for All {
    type Item = Result<FileInfo>;

    fn next(&mut self) -> Option<Result<FileInfo>> {
        loop {
            if let Some(file) = self.page.next() {
                return Some(Ok(file));
            }
            if self.done {
                return None;
            }
            let mut list = self.files.list();
            if let Some(after) = self.after.take() {
                list = list.after(after);
            }
            match list.page() {
                Ok(page) => {
                    self.done = page.next.is_none();
                    self.after = page.next;
                    self.page = page.files.into_iter();
                }
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
