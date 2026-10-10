//! The SQLite every engine stands on: the pinned build, one writer a file with
//! grouped commits, a pool of `query_only` readers, checked migrations.
//!
//! Nothing here knows an engine's vocabulary. `unsafe` is allowed in this
//! module alone, and only in [`memory`].

// The engines that use the adapter land one by one; until kv does, its calls
// are reached only from tests.
#![allow(dead_code, unused_imports)]

mod config;
mod connection;
mod constraint;
mod file;
mod group;
mod memory;
mod migrate;
mod readers;

use std::fmt;
use std::ops::Deref;

use rusqlite::Connection;

pub(crate) use config::{Config, Durability, GroupLimits};
pub(crate) use connection::{execute, sql_error};
pub(crate) use constraint::named as name_constraint;
pub(crate) use file::File;
pub(crate) use memory::used as memory_used;
pub(crate) use migrate::Migration;

/// The writer inside a transaction: a grouped write's savepoint, or a
/// transaction of its own. It derefs to the connection, so an engine prepares
/// and runs its statements on it as on any connection.
pub(crate) struct Tx<'a> {
    connection: &'a Connection,
}

impl<'a> Tx<'a> {
    pub(crate) fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }
}

impl Deref for Tx<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.connection
    }
}

impl fmt::Debug for Tx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Tx")
    }
}
