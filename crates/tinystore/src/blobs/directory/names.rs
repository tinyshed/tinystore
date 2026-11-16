//! A folder's names in the byte order of whole paths, as a scan walks them:
//! sorted in memory up to a few thousand, and past that read into a table of
//! the scan's own, which SQLite sorts, and walked a page at a time. A folder
//! of a million files then holds a buffer of names in memory, never the
//! million (BLOB-16).
//!
//! A folder's key is its name and a `/`, so that what is under it sorts where
//! its paths do: `a-c` < `a/b` < `a0`.

use rusqlite::{Connection, OpenFlags, params};

use crate::sqlite::sql_error;
use crate::{Error, Result};

/// The names a folder sorts in memory; past them it goes to the table.
#[cfg(not(test))]
pub(super) const IN_MEMORY: usize = 4096;
#[cfg(test)]
pub(super) const IN_MEMORY: usize = 64;

/// What a folder's entry is to a scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    File {
        size: u64,
        modified: i64,
    },
    Folder,
    /// A folder it cannot enter, or a link: what the list holds under it stays.
    Kept,
}

#[derive(Debug)]
pub(super) struct Name {
    /// The name, and a `/` after a folder's or a kept one's.
    pub(super) key: String,
    pub(super) kind: Kind,
}

const SCHEMA: &str = "create table names (
    depth    integer not null,
    key      text    not null,
    kind     integer not null,
    size     integer not null,
    modified integer not null,
    primary key (depth, key)
) strict, without rowid";
const INSERT: &str = "insert or replace into names values (?1, ?2, ?3, ?4, ?5)";
const PAGE: &str = "select key, kind, size, modified from names
    where depth = ?1 and key > ?2 order by key limit cast(?3 as integer)";
const CLEAR: &str = "delete from names where depth = ?1";

/// The scan's table of names, opened the first time a folder needs it.
#[derive(Default)]
pub(super) struct Names {
    table: Option<Connection>,
}

/// A folder's names in order, from memory or a page of the table at a time.
pub(super) enum Sorted {
    Memory(std::vec::IntoIter<Name>),
    Table { depth: i64, after: String, page: std::vec::IntoIter<Name>, ended: bool },
}

impl Names {
    /// The names of one folder at `depth`, sorted: what `names` gives is held
    /// in memory up to [`IN_MEMORY`], then a few thousand at a time.
    pub(super) fn sort(&mut self, depth: usize, names: impl Iterator<Item = Name>) -> Result<Sorted> {
        let mut held = Vec::new();
        let mut in_table = false;
        let depth = i64::try_from(depth).unwrap_or(i64::MAX);
        for name in names {
            held.push(name);
            #[cfg(test)]
            MOST_HELD.with(|most| most.set(most.get().max(held.len())));
            if held.len() > IN_MEMORY {
                self.insert(depth, &mut held)?;
                in_table = true;
            }
        }
        if !in_table {
            held.sort_unstable_by(|a, b| a.key.as_bytes().cmp(b.key.as_bytes()));
            return Ok(Sorted::Memory(held.into_iter()));
        }
        self.insert(depth, &mut held)?;
        Ok(Sorted::Table { depth, after: String::new(), page: Vec::new().into_iter(), ended: false })
    }

    /// The next name of a folder in order, none after its last.
    pub(super) fn next(&mut self, sorted: &mut Sorted) -> Result<Option<Name>> {
        match sorted {
            Sorted::Memory(names) => Ok(names.next()),
            Sorted::Table { depth, after, page, ended } => {
                if let Some(name) = page.next() {
                    return Ok(Some(name));
                }
                if *ended {
                    return Ok(None);
                }
                let read = self.page(*depth, after)?;
                *ended = read.len() < IN_MEMORY;
                if let Some(last) = read.last() {
                    after.clone_from(&last.key);
                }
                *page = read.into_iter();
                Ok(page.next())
            }
        }
    }

    /// Empties the table of a folder's names once it is walked.
    pub(super) fn done(&mut self, sorted: &Sorted) -> Result<()> {
        let (Sorted::Table { depth, .. }, Some(table)) = (sorted, &self.table) else { return Ok(()) };
        table.prepare_cached(CLEAR).and_then(|mut clear| clear.execute([depth])).map(drop).map_err(failed)
    }

    fn insert(&mut self, depth: i64, held: &mut Vec<Name>) -> Result<()> {
        let table = self.table()?;
        let tx = table.transaction().map_err(failed)?;
        {
            let mut insert = tx.prepare_cached(INSERT).map_err(failed)?;
            for name in held.drain(..) {
                let (kind, size, modified) = match name.kind {
                    Kind::File { size, modified } => (0, size.cast_signed(), modified),
                    Kind::Folder => (1, 0, 0),
                    Kind::Kept => (2, 0, 0),
                };
                insert.execute(params![depth, name.key, kind, size, modified]).map_err(failed)?;
            }
        }
        tx.commit().map_err(failed)
    }

    fn page(&mut self, depth: i64, after: &str) -> Result<Vec<Name>> {
        let table = self.table()?;
        let limit = i64::try_from(IN_MEMORY).unwrap_or(i64::MAX);
        let mut select = table.prepare_cached(PAGE).map_err(failed)?;
        let rows = select
            .query_map(params![depth, after, limit], |row| {
                let kind = match row.get::<_, i64>(1)? {
                    0 => Kind::File { size: row.get::<_, i64>(2)?.cast_unsigned(), modified: row.get(3)? },
                    1 => Kind::Folder,
                    _ => Kind::Kept,
                };
                Ok(Name { key: row.get(0)?, kind })
            })
            .map_err(failed)?;
        rows.collect::<rusqlite::Result<Vec<Name>>>().map_err(failed)
    }

    /// A private temporary database, which SQLite deletes at its close: its
    /// pages in a cache of 2 MiB and the rest in a file of the system's
    /// temporary folder, never under the directory's.
    fn table(&mut self) -> Result<&mut Connection> {
        if self.table.is_none() {
            let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_EXRESCODE;
            let open = || -> rusqlite::Result<Connection> {
                let table = Connection::open_with_flags("", flags)?;
                table.pragma_update(None, "journal_mode", "off")?;
                table.pragma_update(None, "synchronous", "off")?;
                table.pragma_update(None, "cache_size", -2048)?;
                table.pragma_update(None, "temp_store", "file")?;
                table.execute_batch(SCHEMA)?;
                Ok(table)
            };
            self.table = Some(open().map_err(failed)?);
        }
        self.table.as_mut().ok_or_else(|| Error::internal("a scan's table of names"))
    }
}

fn failed(error: rusqlite::Error) -> Error {
    sql_error("blobs: a scan's table of names", error)
}

#[cfg(test)]
thread_local! {
    /// The most names a folder held in memory on this thread.
    pub(super) static MOST_HELD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
