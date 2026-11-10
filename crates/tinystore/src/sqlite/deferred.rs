//! Whether a transaction's deferred foreign keys stand broken.
//!
//! SQLite checks a foreign key declared `deferrable initially deferred` only
//! as the transaction commits, never as a savepoint inside it is released:
//! a write that breaks one passes its own savepoint, and the commit it shares
//! with other writes then fails whole. So the adapter asks SQLite first, the
//! one question no call of rusqlite asks, and this is the place beside
//! [`super::memory`] and [`super::mutex`] where it writes `unsafe`.

#![allow(unsafe_code)]

use std::ffi::c_int;

use rusqlite::{Connection, ffi};

use super::connection::sql_error;
use crate::Result;

/// Fails `invalid`, as a foreign key SQLite checks at once fails, while one
/// the open transaction on `connection` defers to its commit stands broken:
/// that commit would fail. `who` is the write or the transaction asking.
pub(crate) fn kept(connection: &Connection, who: &str) -> Result<()> {
    if !broken(connection) {
        return Ok(());
    }
    let failure = rusqlite::Error::SqliteFailure(
        ffi::Error::new(ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
        Some("FOREIGN KEY constraint failed".to_owned()),
    );
    Err(sql_error(format!("{who}: a foreign key it deferred to its commit"), failure))
}

/// Whether SQLite counts broken any foreign key that the open transaction on
/// `connection` defers, in the whole of it. A build that does not know the
/// question answers an error, and the commit finds the key, as it did before.
fn broken(connection: &Connection) -> bool {
    let (mut current, mut highest): (c_int, c_int) = (0, 0);
    // SAFETY: the handle is `connection`'s, open while it is borrowed here,
    // and run on by this thread alone, since `Connection` is not `Sync`: no
    // other thread can step a statement on it during the call, which matters
    // as connections are opened without SQLite's own mutex. sqlite3_db_status
    // reads two counters of the handle, resets nothing, and writes two ints
    // on this stack, which outlive the call.
    let asked = unsafe {
        ffi::sqlite3_db_status(connection.handle(), ffi::SQLITE_DBSTATUS_DEFERRED_FKS, &mut current, &mut highest, 0)
    };
    asked == ffi::SQLITE_OK && current != 0
}
