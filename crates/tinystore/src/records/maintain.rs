//! What the engine does on its own schedule: the events their streams keep
//! no longer are removed.

use std::sync::PoisonError;

use rusqlite::params;

use super::engine::{Records, execute, nanos};
use crate::sqlite::sql_error;
use crate::{Result, Store};

/// What one pass of maintenance did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Maintenance {
    /// The events removed, which their streams keep no longer.
    pub expired: u64,
}

/// Runs the store's records maintenance now, as a store opened without
/// background work has to.
pub fn maintain(store: &Store) -> Result<Maintenance> {
    Records::of(store)?.maintain()
}

const COUNT_EXPIRED: &str = "select coalesce(sum(count), 0) from heads where stream = ?1 and last_at < ?2";
const DROP_EXPIRED: &str = "delete from heads where stream = ?1 and last_at < ?2";

impl Records {
    /// One pass: expire, and let the appends' zstd context go, which the next
    /// append makes again.
    pub(crate) fn maintain(&self) -> Result<Maintenance> {
        let now = self.now();
        let failed = |error| sql_error("count what streams keep no longer", error);
        // read first: a store with nothing to remove commits nothing
        let past: Vec<(i64, i64, i64)> = self.file.read(|connection| {
            let mut count = connection.prepare_cached(COUNT_EXPIRED).map_err(failed)?;
            let mut past = Vec::new();
            for stream in self.every_stream() {
                let cutoff = now.saturating_sub(nanos(stream.keeps()));
                let expired: i64 = count.query_row(params![stream.id, cutoff], |row| row.get(0)).map_err(failed)?;
                if expired > 0 {
                    past.push((stream.id, cutoff, expired));
                }
            }
            Ok(past)
        })?;
        self.codec.lock().unwrap_or_else(PoisonError::into_inner).rest();
        if past.is_empty() {
            return Ok(Maintenance::default());
        }
        let expired = past.iter().map(|(_, _, events)| events.max(&0).unsigned_abs()).sum();
        self.file.write(0, move |tx| {
            past.iter().try_for_each(|(stream, cutoff, _)| {
                execute(tx, DROP_EXPIRED, params![stream, cutoff], "remove what a stream keeps no longer").map(drop)
            })
        })?;
        Ok(Maintenance { expired })
    }
}

#[cfg(test)]
#[path = "maintain_tests.rs"]
mod tests;
