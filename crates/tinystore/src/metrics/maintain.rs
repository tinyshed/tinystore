//! What the engine does on its own schedule: what names keep no longer is
//! removed, and what is ready is packed. A pass looks at a bounded number of
//! series, so that it stays short, and another follows at once while one
//! leaves work.

use super::engine::Engine;
use super::retention::PASS_SERIES;
use crate::sync::lock;
use crate::{Result, Store};

/// What maintenance did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Maintenance {
    /// The samples removed, which their names keep no longer.
    pub expired: u64,
    /// The series removed once nothing was left of them, which freed their
    /// places among the store's.
    pub reclaimed: u64,
    /// The samples packed into blocks, and the blocks they made.
    pub packed: u64,
    pub blocks: u64,
    /// The series that stopped taking samples, since their bytes no longer
    /// read: `damaged` lists them.
    pub suspended: u64,
}

/// Runs the store's metrics maintenance until nothing is left to do, as a
/// store opened without background work has to.
pub fn maintain(store: &Store) -> Result<Maintenance> {
    let engine = Engine::of(store)?;
    let mut done = Maintenance::default();
    loop {
        let (pass, unfinished) = engine.pass()?;
        done.expired += pass.expired;
        done.reclaimed += pass.reclaimed;
        done.packed += pass.packed;
        done.blocks += pass.blocks;
        done.suspended += pass.suspended;
        if !unfinished {
            return Ok(done);
        }
    }
}

impl Engine {
    /// One pass, and one at a time; whether it left work for the next.
    pub(super) fn pass(&self) -> Result<(Maintenance, bool)> {
        let _one = lock(&self.maintaining);
        let now = self.now();
        let due = self.due(now)?;
        let more_due = due.len() == PASS_SERIES;
        let expired = self.expire(due)?;
        let (packed, more_ready) = self.pack(now)?;
        self.codecs.release();
        let done = Maintenance {
            expired: expired.samples,
            reclaimed: expired.reclaimed,
            packed: packed.samples,
            blocks: packed.blocks,
            suspended: packed.suspended,
        };
        Ok((done, more_due || more_ready))
    }
}

#[cfg(test)]
#[path = "maintain_tests.rs"]
mod tests;
