//! The SQLite every engine stands on: the pinned build, one writer a file with
//! grouped commits, a pool of `query_only` readers, checked migrations.
//!
//! Nothing here knows an engine's vocabulary. `unsafe` is allowed in this
//! module alone, and only in [`memory`] and [`mutex`].

// Every engine stands on the adapter, and a build of a few of them leaves the
// rest of it unused: there the lint is quiet, while a build of every engine
// still finds what nothing calls.
#![cfg_attr(
    not(all(
        feature = "kv",
        feature = "locks",
        feature = "jobs",
        feature = "sql",
        feature = "blobs",
        feature = "records",
        feature = "metrics",
        feature = "search",
        feature = "backup",
        feature = "replication",
        feature = "wire"
    )),
    allow(dead_code, unused_imports)
)]

mod apply;
mod capture;
mod config;
mod connection;
mod constraint;
mod file;
mod given;
mod group;
mod memory;
mod migrate;
mod mutex;
mod pages;
mod prefix;
mod readers;
mod settle;

use std::cell::RefCell;
use std::fmt;
use std::ops::Deref;

use rusqlite::Connection;

pub(crate) use apply::{Applier, checkpoint_away};
pub(crate) use capture::{Commit, Observe, Position, answering, commits_past, own_work, wal_end, wal_of};
pub use config::Durability;
pub(crate) use config::{Config, GroupLimits};
pub(crate) use connection::{copy, execute_cached, query_cached, quote_ident, read_copy, sql_error};
pub(crate) use constraint::named as name_constraint;
#[cfg(feature = "replication")]
pub(crate) use file::Switch;
pub(crate) use file::{Begun, File, Mark, a_copy_takes_no_transaction, in_a_transaction};
pub(crate) use given::given;
#[cfg(test)]
pub(crate) use memory::used as memory_used;
pub(crate) use migrate::Migration;
pub(crate) use mutex::{give as give_mutexes, given as mutexes_given};
pub(crate) use pages::{Chunk, Pages, Snapshot};
pub(crate) use prefix::{TextBytes, prefix_end};
#[cfg(feature = "replication")]
pub(crate) use readers::Readers;
pub(crate) use settle::{Left, Settle, Settlers};

/// The writer inside a transaction: a grouped write's savepoint, or a
/// transaction of its own. It derefs to the connection, so an engine prepares
/// and runs its statements on it as on any connection.
pub(crate) struct Tx<'a> {
    connection: &'a Connection,
    committed: &'a Committed,
    settlers: &'a Settlers,
}

impl<'a> Tx<'a> {
    pub(crate) fn new(connection: &'a Connection, committed: &'a Committed, settlers: &'a Settlers) -> Self {
        Self { connection, committed, settlers }
    }

    /// Brings the engines kept in the file up to date with what `left`'s
    /// writes left them, before the transaction commits.
    pub(crate) fn settle(&self, left: Left) -> crate::Result<()> {
        self.settlers.run(self, left)
    }

    /// Runs `hook` once the transaction's commit is in the file, while its
    /// writer is still held, so that what two commits of a file hand over
    /// comes in the order they committed: returning from them on two threads
    /// could hand B's over before A's. A rollback runs nothing, and a call
    /// whose savepoint rolled back takes back what it asked for. The hook
    /// must not block: every commit after it waits.
    #[cfg(feature = "channels")]
    pub(crate) fn after_commit(&self, hook: Box<dyn FnOnce()>) {
        self.committed.push(hook);
    }

    pub(crate) fn committed(&self) -> &Committed {
        self.committed
    }
}

/// What a transaction's calls asked to run once it commits, in their order.
#[derive(Default)]
pub(crate) struct Committed(RefCell<Vec<Box<dyn FnOnce()>>>);

impl Committed {
    #[cfg(feature = "channels")]
    pub(crate) fn push(&self, hook: Box<dyn FnOnce()>) {
        self.0.borrow_mut().push(hook);
    }

    /// The hooks asked for so far, taken out.
    #[cfg(feature = "channels")]
    pub(crate) fn take(&self) -> Committed {
        Committed(RefCell::new(std::mem::take(&mut *self.0.borrow_mut())))
    }

    /// Where the next call's hooks begin, to take them back if it fails.
    pub(crate) fn mark(&self) -> usize {
        self.0.borrow().len()
    }

    /// Takes back the hooks asked for since `mark`: their call rolled back.
    pub(crate) fn take_back(&self, mark: usize) {
        self.0.borrow_mut().truncate(mark);
    }

    /// Adds the hooks of another, after its own: a transaction that began on a
    /// file once calls before it had asked for some.
    #[cfg(feature = "channels")]
    pub(crate) fn append(&self, earlier: Committed) {
        let mut hooks = earlier.0.into_inner();
        hooks.append(&mut self.0.borrow_mut());
        *self.0.borrow_mut() = hooks;
    }

    /// Runs the hooks, once the commit is in the file.
    pub(crate) fn run(&self) {
        let hooks = std::mem::take(&mut *self.0.borrow_mut());
        for hook in hooks {
            hook();
        }
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
