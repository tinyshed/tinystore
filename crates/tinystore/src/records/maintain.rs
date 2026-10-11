//! What the engine does on its own schedule: the events their streams keep
//! no longer are removed, the heads that are ready are packed, and the small
//! segments of the streams packed are merged.

use std::sync::PoisonError;

use rusqlite::{Connection, params};

use super::engine::{Records, execute, nanos};
use super::pack::settle;
use crate::sqlite::sql_error;
use crate::{Result, Store};

/// What one pass of maintenance did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Maintenance {
    /// The events removed, which their streams keep no longer.
    pub expired: u64,
    /// The events packed, and the segments they made.
    pub packed: u64,
    pub segments: u64,
    /// The segments merged into others, which keep their places.
    pub merged: u64,
}

/// Runs the store's records maintenance now, as a store opened without
/// background work has to.
pub fn maintain(store: &Store) -> Result<Maintenance> {
    Records::of(store)?.maintain()
}

/// What a stream keeps no longer: the events of its head rows and of its
/// segments whose newest event is before `?2`, a segment's places with it.
const COUNT_EXPIRED: &str = "select
    (select coalesce(sum(count), 0) from heads where stream = ?1 and last_at < ?2),
    (select coalesce(sum(held), 0) from segments where stream = ?1 and holder is null and last_at < ?2)";
const EXPIRED_HEADS: &str =
    "select late, sum(count), sum(weight) from heads where stream = ?1 and last_at < ?2 group by late";
const DROP_HEADS: &str = "delete from heads where stream = ?1 and last_at < ?2";
const DROP_TRACES: &str = "delete from block_traces where block in (select id from blocks where segment in
    (select id from segments where stream = ?1 and holder is null and last_at < ?2))";
const DROP_FILTERS: &str = "delete from block_filters where block in (select id from blocks where segment in
    (select id from segments where stream = ?1 and holder is null and last_at < ?2))";
const DROP_KEYS: &str = "delete from segment_keys where segment in
    (select id from segments where stream = ?1 and holder is null and last_at < ?2)";
const DROP_BLOCKS: &str = "delete from blocks where segment in
    (select id from segments where stream = ?1 and holder is null and last_at < ?2)";
const DROP_PLACES: &str = "delete from segments where holder in
    (select id from segments where stream = ?1 and holder is null and last_at < ?2)";
const DROP_SEGMENTS: &str = "delete from segments where stream = ?1 and holder is null and last_at < ?2";

impl Records {
    /// One pass, and one at a time: expire, pack what is ready, merge the
    /// small segments of the streams packed, and let the appends' zstd
    /// context go, which the next append makes again.
    pub(crate) fn maintain(&self) -> Result<Maintenance> {
        let _one = self.maintaining.lock().unwrap_or_else(PoisonError::into_inner);
        let now = self.now();
        let expired = self.expire(now)?;
        let packing = self.pack(now)?;
        let merged = self.merge(packing.streams)?;
        self.codec.lock().unwrap_or_else(PoisonError::into_inner).rest();
        Ok(Maintenance { expired, packed: packing.events, segments: packing.segments, merged })
    }

    /// Removes what each stream keeps no longer: a head row or a segment
    /// goes whole, once its newest event is past what its stream keeps, and
    /// a segment with its places.
    fn expire(&self, now: i64) -> Result<u64> {
        let failed = |error| sql_error("count what streams keep no longer", error);
        // read first: a store with nothing to remove commits nothing
        let past: Vec<(i64, i64, u64)> = self.file.read(|connection| {
            let mut count = connection.prepare_cached(COUNT_EXPIRED).map_err(failed)?;
            let mut past = Vec::new();
            for stream in self.every_stream() {
                let cutoff = now.saturating_sub(nanos(stream.keeps()));
                let counted = count.query_row(params![stream.id, cutoff], |row| Ok((row.get(0)?, row.get(1)?)));
                let (fresh, packed): (i64, i64) = counted.map_err(failed)?;
                if fresh + packed > 0 {
                    past.push((stream.id, cutoff, (fresh + packed).unsigned_abs()));
                }
            }
            Ok(past)
        })?;
        if past.is_empty() {
            return Ok(0);
        }
        let expired = past.iter().map(|(_, _, events)| events).sum();
        self.file
            .write(0, move |tx| past.iter().try_for_each(|(stream, cutoff, _)| drop_past(tx, *stream, *cutoff)))?;
        Ok(expired)
    }
}

/// Removes a stream's head rows and segments whose newest event is before
/// `cutoff`, a segment's blocks and places with it, and takes the rows from
/// what their heads hold.
fn drop_past(tx: &Connection, stream: i64, cutoff: i64) -> Result<()> {
    let left = tx.prepare_cached(EXPIRED_HEADS).and_then(|mut select| {
        let heads = select.query_map(params![stream, cutoff], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)));
        heads.and_then(Iterator::collect::<rusqlite::Result<Vec<(bool, i64, i64)>>>)
    });
    let left = left.map_err(|error| sql_error("count the head rows a stream keeps no longer", error))?;
    let drops = [
        (DROP_HEADS, "head rows"),
        (DROP_TRACES, "blocks' traces"),
        (DROP_FILTERS, "blocks' filters"),
        (DROP_KEYS, "segments' keys"),
        (DROP_BLOCKS, "blocks"),
        (DROP_PLACES, "places"),
        (DROP_SEGMENTS, "segments"),
    ];
    for (sql, what) in drops {
        execute(tx, sql, params![stream, cutoff], &format!("remove the {what} a stream keeps no longer"))?;
    }
    left.into_iter().try_for_each(|(late, events, weight)| settle(tx, (stream, late), events, weight).map(drop))
}

#[cfg(test)]
#[path = "maintain_tests.rs"]
mod tests;
