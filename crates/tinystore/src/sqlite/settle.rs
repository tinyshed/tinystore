//! What an engine kept in a file brings up to date inside each transaction
//! of it, before the transaction commits: a table's search index, from the
//! keys its triggers left, so that the index commits with the rows or not at
//! all, and kv's revision, the highest version the transaction's writes took.
//!
//! ```text
//! begin ── settle(Before) ── a write ── settle(Here) ── a write ── settle(Here) ── settle(All) ── commit
//! ```
//!
//! A transaction settles what was left before it began, by a process that
//! writes the file beside the store, then what each of its writes leaves,
//! inside that write's savepoint: a write whose work fails to settle fails
//! alone, as its savepoint rolls back. Last, once, what its writes leave
//! together: a grouped commit of 64 sets keeps the revision once, not 64
//! times.

use std::sync::{PoisonError, RwLock, Weak};

use super::Tx;
use crate::Result;

/// Whose writes left the work a settle does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Left {
    /// Writes before the transaction began, by another process: what of
    /// them fails is the engine's to say and leave, since no caller here
    /// made them.
    Before,
    /// The write that just ran, which a failure fails.
    Here,
    /// Every write of the transaction, once the last has run. A failure fails
    /// them all, so it settles only what no one write can fail.
    All,
}

/// An engine kept in a file whose transactions settle it.
pub(crate) trait Settle: Send + Sync {
    /// Brings the engine up to date inside `tx`. It runs on the writer, so it
    /// waits for nothing that may wait for the writer.
    fn settle(&self, tx: &Tx<'_>, left: Left) -> Result<()>;
}

/// The engines a file's transactions settle: none until one is kept in it,
/// which costs a commit nothing.
#[derive(Default)]
pub(crate) struct Settlers(RwLock<Vec<Weak<dyn Settle>>>);

impl Settlers {
    pub(crate) fn add(&self, settle: Weak<dyn Settle>) {
        self.0.write().unwrap_or_else(PoisonError::into_inner).push(settle);
    }

    pub(crate) fn run(&self, tx: &Tx<'_>, left: Left) -> Result<()> {
        let settlers = self.0.read().unwrap_or_else(PoisonError::into_inner);
        for settle in settlers.iter().filter_map(Weak::upgrade) {
            settle.settle(tx, left)?;
        }
        Ok(())
    }
}
