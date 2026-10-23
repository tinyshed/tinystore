//! A file's free pages given back to the disk. A delete leaves the pages it
//! emptied in the file, on its free list, where the next writes take their
//! pages from first; SQLite gives them back to the disk only in a file made
//! with `auto_vacuum = incremental`, and only when asked, a page at a time.
//!
//! The setting takes effect on a file that has no table yet. A file made
//! without it, as every file of v0.1.0 was, becomes one only through a
//! `vacuum`, which writes the whole file again while it holds the writer:
//!
//! | The file                                       | What a give back does                                 |
//! |------------------------------------------------|-------------------------------------------------------|
//! | made `incremental`                             | gives back the pages free for `UNNEEDED_FOR`, a bound |
//! | made without it, `VACUUM_UP_TO` in use at most | vacuums it once, which makes it `incremental`         |
//! | made without it and larger, or a node logs it  | nothing: its free pages wait for its next writes      |
//! | a copy                                         | nothing: its pages are its home's                     |
//!
//! A page given back moves the file's last page into a free one, its parent
//! and its entry in the pointer map rewritten, and cuts the file by a page:
//! a few pages written for each page given back, which a node's log takes as
//! any commit's, the file's new size with them. A vacuum would hand the log
//! the whole file in one commit, which is why a file a node logs is never
//! vacuumed.
//!
//! The file is cut in the WAL first and on the disk at the next checkpoint
//! that copies the whole WAL back.

use std::path::Path;
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, OpenFlags};

use super::connection::{execute, sql_error};
use super::file::TRUNCATE;
use super::given::own;
use crate::Result;

/// The pages in use of a file made without `auto_vacuum` that one vacuum
/// rewrites at most, to make it incremental: its writer waits while it runs.
pub(super) const VACUUM_UP_TO: u64 = 8 << 20;

/// How long free pages stay free before they are given back. A live stream
/// frees its heads' pages at each pack and takes them back as it appends
/// until the next: given back at once, they would be moved and grown again
/// every cycle. Ten minutes covers the cycle of a stream that packs a
/// segment's worth more often, and costs a slower stream little.
pub(super) const UNNEEDED_FOR: Duration = Duration::from_mins(10);

const PAGE_SIZE: &str = "pragma page_size";
const PAGES: &str = "pragma page_count";
const FREE: &str = "pragma freelist_count";
const MODE: &str = "pragma auto_vacuum";
/// Asks for every free page, a step each: a give back steps it as many
/// times as it may and lets it go.
const GIVE_BACK: &str = own!("pragma incremental_vacuum");
const INCREMENTAL: &str = own!("pragma auto_vacuum = incremental");
const VACUUM: &str = own!("vacuum");

/// What `auto_vacuum` says of a file: 2 is incremental.
const MODE_INCREMENTAL: i64 = 2;

/// How a file stands: its pages, those free among them, and whether it can
/// give the free ones back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Occupancy {
    pub(super) page_size: u64,
    pub(super) pages: u64,
    pub(super) free: u64,
    pub(super) incremental: bool,
}

impl Occupancy {
    pub(super) fn read(connection: &Connection) -> Result<Occupancy> {
        let one = |sql: &str| -> Result<i64> {
            connection.query_row(sql, [], |row| row.get(0)).map_err(|error| sql_error("its pages", error))
        };
        let (page_size, pages, free) = (one(PAGE_SIZE)?, one(PAGES)?, one(FREE)?);
        let count = |value: i64| value.max(0).unsigned_abs();
        Ok(Occupancy {
            page_size: count(page_size).max(1),
            pages: count(pages),
            free: count(free),
            incremental: one(MODE)? == MODE_INCREMENTAL,
        })
    }

    /// The bytes of the pages that hold something.
    pub(super) fn in_use(&self) -> u64 {
        self.pages.saturating_sub(self.free) * self.page_size
    }

    /// The free pages `most` bytes give back at most.
    pub(super) fn free_within(self, most: usize) -> u64 {
        self.free.min(most as u64 / self.page_size)
    }
}

/// The free pages a file did not need for `UNNEEDED_FOR`: the fewest it held
/// at any give back of a window that long, which the give backs of the next
/// window may take, a bound at a time.
#[derive(Debug, Default)]
pub(super) struct Unneeded {
    /// When the window began, and the fewest free pages seen in it.
    window: Option<(SystemTime, u64)>,
    /// What the last window found unneeded and is not given back yet.
    owed: u64,
}

impl Unneeded {
    /// Notes the `free` pages a give back finds at `now`; how many of them
    /// it may give back.
    pub(super) fn note(&mut self, now: SystemTime, free: u64) -> u64 {
        let (began, fewest) = self.window.unwrap_or((now, free));
        let fewest = fewest.min(free);
        self.window = match now.duration_since(began) {
            Ok(lasted) if lasted < UNNEEDED_FOR => Some((began, fewest)),
            Ok(_) => {
                self.owed = fewest;
                Some((now, free))
            }
            // a clock moved back begins the window again
            Err(_) => Some((now, free)),
        };
        self.owed.min(free)
    }

    /// `pages` of the free pages were given back.
    pub(super) fn given(&mut self, pages: u64) {
        self.owed = self.owed.saturating_sub(pages);
        if let Some((_, fewest)) = &mut self.window {
            *fewest = fewest.saturating_sub(pages);
        }
    }
}

/// Gives back `pages` of the file's free pages, fewer when it has fewer, in
/// the transaction `connection` is in; how many it gave back.
pub(super) fn give_back(connection: &Connection, pages: u64) -> Result<u64> {
    let failed = |error| sql_error("give its free pages back", error);
    let mut statement = connection.prepare_cached(GIVE_BACK).map_err(failed)?;
    let mut steps = statement.query([]).map_err(failed)?;
    let mut given = 0;
    // A step that gives no page back ends the statement: none is left.
    while given < pages && steps.next().map_err(failed)?.is_some() {
        given += 1;
    }
    Ok(given)
}

/// Writes the file at `path` again as one made `incremental`, its free
/// pages left out, and empties the WAL that then holds all of it; the pages
/// the file gave back. Its writer must be held and in no transaction.
///
/// A vacuum attaches a database of its own to write the file into, which no
/// connection of the store's may: it runs on a connection of its own, as a
/// copy's `vacuum into` does, and the file's connections read the file it
/// made at their next transaction.
pub(super) fn vacuum(path: &Path, busy_timeout: Duration) -> Result<u64> {
    super::give_mutexes();
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_EXRESCODE;
    let connection = Connection::open_with_flags(path, flags).map_err(|error| sql_error("open it", error))?;
    connection.busy_timeout(busy_timeout).map_err(|error| sql_error("open it", error))?;
    let before = Occupancy::read(&connection)?.pages;
    execute(&connection, INCREMENTAL).map_err(|error| sql_error("make it incremental", error))?;
    execute(&connection, VACUUM).map_err(|error| sql_error("vacuum it", error))?;
    // A reader that holds the WAL leaves it as large as the file till the
    // next checkpoint that empties it: the store's close, at the latest.
    connection.query_row(TRUNCATE, [], |_| Ok(())).map_err(|error| sql_error("checkpoint its vacuum", error))?;
    Ok(before.saturating_sub(Occupancy::read(&connection)?.pages))
}

#[cfg(test)]
#[path = "vacuum_tests.rs"]
mod tests;
