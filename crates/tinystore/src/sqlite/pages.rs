//! A file's pages read as they lie, a chunk at a time, each chunk in a read
//! transaction of its own: what the first copy of a file on another node is
//! made from. A reader held open for the whole file would keep the WAL from
//! starting over for as long as the copy takes.
//!
//! Chunks read at different commits do not make one file. The copy applies
//! the commits that ended while it read on top of them, and each of those
//! carries every page it wrote, so a page read before its last change is
//! written again.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::Config;
use super::connection::{self, Role, execute, sql_error};
use super::given::own;
use crate::{Error, ErrorKind, Result};

const BEGIN: &str = own!("begin");
const END: &str = own!("commit");
const PAGE: &str = "select data from sqlite_dbpage where pgno = ?1";
const PAGES: &str = "pragma page_count";
const PAGE_SIZE: &str = "pragma page_size";

/// A reader of a file's pages by their numbers.
pub(crate) struct Pages {
    path: PathBuf,
    connection: Connection,
}

/// One read transaction over a whole file, its snapshot pinned at the commit
/// the file stood at when it began: what a checksum of the file reads. It
/// ends when dropped.
pub(crate) struct Snapshot<'a> {
    pages: &'a Pages,
    size: u32,
}

/// Pages read in one transaction, and the file's size at it.
pub(crate) struct Chunk {
    pub(crate) page_size: u32,
    /// The file's pages at the chunk's commit.
    pub(crate) size: u32,
    pub(crate) pages: Vec<(u32, Vec<u8>)>,
}

impl Pages {
    pub(crate) fn open(path: &Path) -> Result<Pages> {
        // a node's file no engine holds: its WAL holds what its log may lack
        let config = Config { keep_wal: true, ..Config::default() };
        let connection = connection::open(path, &config, Role::Reader)?;
        refuse_without_dbpage(&connection, path)?;
        Ok(Pages { path: path.to_owned(), connection })
    }

    /// Begins a read transaction and pins its snapshot with a first read, so
    /// that a caller holding the file still while it begins reads that commit.
    pub(crate) fn begin(&self) -> Result<Snapshot<'_>> {
        let what = || format!("{}: its snapshot", self.path.display());
        execute(&self.connection, BEGIN).map_err(|error| sql_error(what(), error))?;
        match self.connection.query_row(PAGES, [], |row| row.get(0)) {
            Ok(size) => Ok(Snapshot { pages: self, size }),
            Err(error) => {
                let _ = execute(&self.connection, END);
                Err(sql_error(what(), error))
            }
        }
    }

    /// Pages from `first`, `count` at most and none past the file's end, read
    /// in one transaction.
    pub(crate) fn read(&self, first: u32, count: u32) -> Result<Chunk> {
        let what = || format!("{}: its pages from {first}", self.path.display());
        execute(&self.connection, BEGIN).map_err(|error| sql_error(what(), error))?;
        let chunk = self.read_inside(first, count).map_err(|error| sql_error(what(), error));
        let ended = execute(&self.connection, END).map_err(|error| sql_error(what(), error));
        let chunk = chunk?;
        ended?;
        Ok(chunk)
    }

    fn read_inside(&self, first: u32, count: u32) -> rusqlite::Result<Chunk> {
        // its size and pages are read in the transaction `read` began
        let page_size: u32 = self.connection.query_row(PAGE_SIZE, [], |row| row.get(0))?;
        let size: u32 = self.connection.query_row(PAGES, [], |row| row.get(0))?;
        let last = first.saturating_add(count).min(size.saturating_add(1));
        let mut statement = self.connection.prepare_cached(PAGE)?;
        let mut pages = Vec::with_capacity(last.saturating_sub(first) as usize);
        for number in first..last {
            let page: Vec<u8> = statement.query_row([number], |row| row.get(0))?;
            pages.push((number, page));
        }
        Ok(Chunk { page_size, size, pages })
    }
}

impl Snapshot<'_> {
    /// Hands each page of the snapshot to `each`, in order.
    pub(crate) fn each(&self, mut each: impl FnMut(u32, &[u8])) -> Result<()> {
        let what = || format!("{}: its pages", self.pages.path.display());
        let mut statement = self.pages.connection.prepare_cached(PAGE).map_err(|error| sql_error(what(), error))?;
        for number in 1..=self.size {
            let page: Vec<u8> =
                statement.query_row([number], |row| row.get(0)).map_err(|error| sql_error(what(), error))?;
            each(number, &page);
        }
        Ok(())
    }
}

impl Drop for Snapshot<'_> {
    fn drop(&mut self) {
        let _ = execute(&self.pages.connection, END);
    }
}

/// Refuses a connection whose SQLite has no `sqlite_dbpage`, through which a
/// node reads and writes a file's pages.
pub(super) fn refuse_without_dbpage(connection: &Connection, path: &Path) -> Result<()> {
    let found: i64 = connection
        .query_row("select count(*) from pragma_module_list where name = 'sqlite_dbpage'", [], |row| row.get(0))
        .map_err(|error| sql_error(path.display().to_string(), error))?;
    if found == 0 {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!(
                "{}: the SQLite linked in has no sqlite_dbpage, through which a node reads and writes pages: build                  with LIBSQLITE3_FLAGS holding SQLITE_ENABLE_DBPAGE_VTAB, as this workspace does",
                path.display()
            ),
        ));
    }
    Ok(())
}
