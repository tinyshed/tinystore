//! What the last scan wrote down of a directory: a file's line, a page of a
//! folder's files, and what they count. None of it touches the disk.

use std::sync::Arc;

use rusqlite::{OptionalExtension, params};

use super::{Directory, version_info};
use crate::blobs::engine::Blobs;
use crate::blobs::info::{FileInfo, Page, Usage};
use crate::blobs::list::{PAGE_FILES, PAST_EVERY};
use crate::blobs::paths::Folder;
use crate::sqlite::sql_error;
use crate::{Error, Result};

const SELECT_ROW: &str = "select path, size, modified from _tinystore_blobs_listed
    where directory = ?1 and path = ?2 and removed is null";
const SELECT_PAGE: &str = "select path, size, modified from _tinystore_blobs_listed
    where directory = ?1 and path >= ?2 and path < ?3 and removed is null order by path limit cast(?4 as integer)";
const SELECT_USAGE: &str = "select count(*), coalesce(sum(size), 0) from _tinystore_blobs_listed
    where directory = ?1 and path >= ?2 and path < ?3 and removed is null";

/// A file's line as the list holds it.
#[derive(Debug)]
pub(super) struct Row {
    pub(super) path: String,
    pub(super) size: u64,
    pub(super) modified: i64,
}

impl Row {
    pub(super) fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
        Ok(Row { path: row.get(0)?, size: row.get::<_, i64>(1)?.cast_unsigned(), modified: row.get(2)? })
    }

    pub(super) fn info(&self, folder: &Folder) -> FileInfo {
        version_info(folder.relative(&self.path), self.size, self.modified)
    }
}

/// The line of the file at a whole path, while the list holds it.
pub(super) fn row(blobs: &Arc<Blobs>, directory: i64, whole: &str) -> Result<Option<Row>> {
    blobs.file.read(|connection| {
        connection
            .prepare_cached(SELECT_ROW)
            .and_then(|mut select| select.query_row(params![directory, whole], Row::read).optional())
            .map_err(|error| sql_error("blobs: a directory's file", error))
    })
}

/// A page of a folder's files, its options before [`DirectoryList::page`].
#[derive(Debug)]
#[must_use = "a list reads nothing until its page"]
pub struct DirectoryList<'d> {
    directory: &'d Directory,
    prefix: String,
    after: Option<String>,
    limit: usize,
}

impl Directory {
    /// A page of this folder's files and of the folders in it, in the byte
    /// order of their paths, as the last scan wrote them down.
    pub fn list(&self) -> DirectoryList<'_> {
        DirectoryList { directory: self, prefix: String::new(), after: None, limit: PAGE_FILES }
    }

    /// The files of this folder and of the folders in it, and their bytes, as
    /// the last scan wrote them down.
    pub fn usage(&self) -> Result<Usage> {
        self.usable()?;
        let (from, to) = self.bounds("").map_err(|error| self.fail(self.folder.prefix(), error))?;
        usage_of(&self.blobs, self.opened.id, &from, &to).map_err(|error| self.fail(self.folder.prefix(), error))
    }

    /// The first path under this folder that starts with `prefix`, and the
    /// first past every such path.
    fn bounds(&self, prefix: &str) -> Result<(String, String)> {
        let (from, to) = self.folder.range(prefix)?;
        Ok((from, to.unwrap_or_else(|| PAST_EVERY.clone())))
    }

    fn page(&self, prefix: &str, after: Option<&str>, limit: usize) -> Result<Page> {
        if limit == 0 || limit > PAGE_FILES {
            return Err(Error::invalid(format!("a page of {limit} files; 1 to {PAGE_FILES}")));
        }
        let (mut from, to) = self.bounds(prefix)?;
        if let Some(after) = after {
            let (past_after, _) = self.folder.range(after)?;
            from = from.max(format!("{past_after}\u{0}"));
        }
        let wanted = i64::try_from(limit + 1).unwrap_or(i64::MAX);
        let rows = self.blobs.file.read(|connection| {
            let failed = |error| sql_error("blobs: a directory's page", error);
            let mut statement = connection.prepare_cached(SELECT_PAGE).map_err(failed)?;
            let rows = statement.query_map(params![self.opened.id, from, to, wanted], Row::read).map_err(failed)?;
            rows.collect::<rusqlite::Result<Vec<Row>>>().map_err(failed)
        })?;
        let more = rows.len() > limit;
        let files: Vec<FileInfo> = rows.iter().take(limit).map(|row| row.info(&self.folder)).collect();
        let next = if more { files.last().map(|file| file.path.clone()) } else { None };
        Ok(Page { files, next })
    }
}

/// The files of a directory between two paths, and their bytes.
pub(super) fn usage_of(blobs: &Arc<Blobs>, directory: i64, from: &str, to: &str) -> Result<Usage> {
    let (count, size): (i64, i64) = blobs.file.read(|connection| {
        connection
            .prepare_cached(SELECT_USAGE)
            .and_then(|mut select| select.query_row(params![directory, from, to], |row| Ok((row.get(0)?, row.get(1)?))))
            .map_err(|error| sql_error("blobs: a directory's usage", error))
    })?;
    Ok(Usage { count: count.cast_unsigned(), size: size.cast_unsigned(), ..Usage::default() })
}

impl DirectoryList<'_> {
    /// Only the files whose path within the folder starts with this text.
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
        let directory = self.directory;
        directory.usable()?;
        directory
            .page(&self.prefix, self.after.as_deref(), self.limit)
            .map_err(|error| directory.fail(directory.folder.prefix(), error))
    }
}
