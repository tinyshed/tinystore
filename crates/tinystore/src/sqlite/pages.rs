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

/// Pages read in one transaction, and the file's size at it.
pub(crate) struct Chunk {
    pub(crate) page_size: u32,
    /// The file's pages at the chunk's commit.
    pub(crate) size: u32,
    pub(crate) pages: Vec<(u32, Vec<u8>)>,
}

impl Pages {
    pub(crate) fn open(path: &Path) -> Result<Pages> {
        let connection = connection::open(path, &Config::default(), Role::Reader)?;
        let found: i64 = connection
            .query_row("select count(*) from pragma_module_list where name = 'sqlite_dbpage'", [], |row| row.get(0))
            .map_err(|error| sql_error(path.display().to_string(), error))?;
        if found == 0 {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!(
                    "{}: the SQLite linked in has no sqlite_dbpage: build with SQLITE_ENABLE_DBPAGE_VTAB",
                    path.display()
                ),
            ));
        }
        Ok(Pages { path: path.to_owned(), connection })
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
