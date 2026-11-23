//! A folder's files: a page of them, every page, what they count, and a clear
//! of them all.

use std::sync::{Arc, LazyLock};

use rusqlite::params;

use super::change::{Change, hidden};
#[cfg(feature = "async")]
use super::files::Done;
use super::files::{Files, Writing};
use super::info::{FileInfo, FileRow, Page, Usage, file_columns};
use super::paths::MAX_PATH;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, Result};

/// Files a page holds at most, and when a list does not say.
pub const PAGE_FILES: usize = 1000;

/// Files a clear deletes in its own transaction; past them it marks the
/// folder and maintenance deletes them.
pub(crate) const CLEAR_AT_ONCE: usize = 10_000;

const SELECT_PAGE: &str = concat!(
    "select ",
    file_columns!(),
    " from _tinystore_blobs_files as f where f.set_id = ?1 and f.path >= ?2 and f.path < ?3
        and (f.expires is null or f.expires > ?4) and not ",
    hidden!(),
    " order by f.path limit cast(?5 as integer)"
);
const SELECT_USAGE: &str = concat!(
    "select count(*), coalesce(sum(f.size), 0) from _tinystore_blobs_files as f where f.set_id = ?1 and f.path >= ?2
        and f.path < ?3 and (f.expires is null or f.expires > ?4) and not ",
    hidden!()
);
const COUNT_UNDER: &str = "select count(*) from (
    select 1 from _tinystore_blobs_files where set_id = ?1 and path >= ?2 and path < ?3 limit cast(?4 as integer))";
const DELETE_UNDER: &str =
    "delete from _tinystore_blobs_files where set_id = ?1 and path >= ?2 and path < ?3 returning content";
const MARK_CLEARED: &str = "insert into _tinystore_blobs_cleared (set_id, prefix, revision) values (?1, ?2, ?3)
    on conflict (set_id, prefix) do update set revision = excluded.revision";

/// Past every path: longer than a path may be, of the last character.
pub(crate) static PAST_EVERY: LazyLock<String> = LazyLock::new(|| "\u{10ffff}".repeat(MAX_PATH / 4 + 1));

/// A page of a folder's files, its options before [`List::page`].
#[derive(Debug)]
#[must_use = "a list reads nothing until its page"]
pub struct List<'f> {
    files: &'f Files,
    /// The transaction whose writes the page sees, for a list made inside one.
    #[cfg(feature = "sql")]
    tx: Option<&'f crate::Transaction<'f>>,
    prefix: String,
    after: Option<String>,
    limit: usize,
}

impl Files {
    /// A page of this folder's files and of the folders in it, in the byte
    /// order of their paths: `10.jpg` before `9.jpg`.
    pub fn list(&self) -> List<'_> {
        List {
            files: self,
            #[cfg(feature = "sql")]
            tx: None,
            prefix: String::new(),
            after: None,
            limit: PAGE_FILES,
        }
    }

    /// [`Files::list`] inside a transaction of the database the files are
    /// kept in: its page sees what the transaction wrote.
    #[cfg(feature = "sql")]
    pub(super) fn list_in<'f>(&'f self, tx: &'f crate::Transaction<'f>) -> List<'f> {
        List { tx: Some(tx), ..self.list() }
    }

    /// Every file of this folder and of the folders in it, a page at a time,
    /// holding nothing between pages.
    pub fn all(&self) -> All {
        All { files: self.clone(), after: None, page: Vec::new().into_iter(), done: false }
    }

    /// The files of this folder and of the folders in it, and their bytes, as
    /// they are now, with the set's ceiling and what it evicted since the
    /// store opened. It renews nothing.
    pub fn usage(&self) -> Result<Usage> {
        let folder = self.whole_folder()?;
        let (from, to) = self.bounds("").map_err(|error| self.fail(&folder, error))?;
        let now = self.blobs.now();
        let ((count, size), ceiling): ((i64, i64), _) = self
            .blobs
            .file
            .read(|connection| {
                let held = connection
                    .prepare_cached(SELECT_USAGE)
                    .and_then(|mut statement| {
                        statement.query_row(params![self.set, from, to, now], |row| Ok((row.get(0)?, row.get(1)?)))
                    })
                    .map_err(|error| sql_error("blobs: a folder's usage", error))?;
                Ok((held, super::cache::kept_on(connection, self.set)?))
            })
            .map_err(|error| self.fail(&folder, error))?;
        Ok(Usage {
            count: u64::try_from(count).unwrap_or(0),
            size: u64::try_from(size).unwrap_or(0),
            max_size: ceiling.map(|ceiling| ceiling.max_size),
            evictions: self.blobs.evictions.of(self.set),
        })
    }

    /// Removes this folder and every folder in it, at once for every reader,
    /// however many files it holds. On the files themselves it empties them.
    pub fn clear(&self) -> Result<()> {
        #[cfg(feature = "replication")]
        if self.is_copy() {
            let folder = self.whole_folder()?;
            let cleared = self.pass_on(&super::forward::Call::Clear { folder: &folder });
            return cleared.map(drop).map_err(|error| self.fail(&folder, error));
        }
        let (folder, clearing) = self.clearing()?;
        let ((), freed) = self.blobs.file.write(0, clearing).map_err(|error| self.fail(&folder, error))?;
        self.blobs.remove_freed(&freed);
        Ok(())
    }

    /// [`Files::clear`] for an awaited write, submitted: `done` is given its
    /// answer once its commit ends, on the thread that answers the commit.
    #[cfg(feature = "async")]
    pub(crate) fn clear_then(&self, done: impl Done<()>) {
        let (folder, clearing) = match self.clearing() {
            Ok(clearing) => clearing,
            Err(error) => return done(Err(error)),
        };
        let files = self.clone();
        self.blobs.file.submit(0, clearing, move |cleared| done(cleared.map_err(|error| files.fail(&folder, error))));
    }

    /// The folder as an error names it, and what its clear writes in its
    /// transaction: the files deleted, or past [`CLEAR_AT_ONCE`] a mark that
    /// hides them until maintenance deletes them.
    pub(super) fn clearing(&self) -> Result<(String, Writing<()>)> {
        let folder = self.whole_folder()?;
        let blobs = Arc::clone(&self.blobs);
        let (set, prefix) = (self.set, self.folder.prefix().to_owned());
        let (from, to) = self.bounds("").map_err(|error| self.fail(&folder, error))?;
        let bound = i64::try_from(CLEAR_AT_ONCE).unwrap_or(i64::MAX);
        let clearing = move |tx: &Tx<'_>| {
            let mut change = Change::new(tx, &blobs);
            change.end_claims_under(set, &from, &to)?;
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
            Ok(((), change.freed))
        };
        Ok((folder, Box::new(clearing)))
    }

    /// The first path under this folder that starts with `prefix`, and the
    /// first past every such path.
    fn bounds(&self, prefix: &str) -> Result<(String, String)> {
        let (from, to) = self.folder.range(prefix)?;
        Ok((from, to.unwrap_or_else(|| PAST_EVERY.clone())))
    }

    /// The folder as an error names it, or why it is not one.
    fn whole_folder(&self) -> Result<String> {
        self.usable().map(|()| self.folder.prefix().to_owned())
    }

    /// A page of the folder as `list` asks for it, read from a snapshot of
    /// the file, or on the writer of the transaction a list was made in.
    fn page(&self, list: &List<'_>) -> Result<Page> {
        let limit = list.limit;
        if limit == 0 || limit > PAGE_FILES {
            return Err(Error::invalid(format!("a page of {limit} files; 1 to {PAGE_FILES}")));
        }
        let (mut from, to) = self.bounds(&list.prefix)?;
        if let Some(after) = &list.after {
            let (past_after, _) = self.folder.range(after)?;
            from = from.max(format!("{past_after}\u{0}"));
        }
        let now = self.blobs.now();
        let wanted = i64::try_from(limit + 1).unwrap_or(i64::MAX);
        let read = |connection: &rusqlite::Connection| {
            let mut statement =
                connection.prepare_cached(SELECT_PAGE).map_err(|error| sql_error("blobs: a folder's page", error))?;
            let rows = statement
                .query_map(params![self.set, from, to, now, wanted], FileRow::read)
                .map_err(|error| sql_error("blobs: a folder's page", error))?;
            rows.collect::<rusqlite::Result<Vec<FileRow>>>().map_err(|error| sql_error("blobs: a folder's page", error))
        };
        #[cfg(feature = "sql")]
        let rows = match list.tx {
            Some(tx) => super::tx::on_file(tx, self, |sql| read(sql))?,
            None => self.blobs.file.read(read)?,
        };
        #[cfg(not(feature = "sql"))]
        let rows = self.blobs.file.read(read)?;
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
        self.files.page(&self).map_err(|error| self.files.fail(&folder, error))
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
