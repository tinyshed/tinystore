//! What the engine does on its own schedule: the pages the file holds free
//! are given back to the disk, the events their streams keep no longer are
//! removed, the heads that are ready are packed, and the small segments of
//! the streams packed are merged.

use std::sync::PoisonError;
use std::time::Duration;

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
/// Head rows keep removed before they were packed take a segment's id of
/// their own, which no segment holds: a poll behind it counts their events,
/// as a segment's that expired.
const TAKE_ID: &str = "insert into segments (stream, count, start, held, weight, body)
    values (?1, ?2, 0, ?2, 0, x'') returning id";
const GIVE_UP_ID: &str = "delete from segments where id = ?1";
const NOTE_EXPIRED_HEADS: &str = "insert into expired (segment, events, at) values (?1, ?2, ?3)";
/// What each segment a stream keeps no longer held, its places' among them.
const NOTE_EXPIRED: &str = "insert or replace into expired (segment, events, at)
    select id, count, ?3 from segments where stream = ?1 and (holder is null and last_at < ?2
    or holder in (select id from segments where stream = ?1 and holder is null and last_at < ?2))";
/// How long the file keeps what a segment it removed held, for the polls
/// behind it.
const EXPIRED_KEPT: Duration = Duration::from_hours(30 * 24);
const FORGET_EXPIRED: &str = "delete from expired where at < ?1";
/// The ids of a stream's events of a span that none of its head rows and
/// segments can hold any more: no span of one holds their time. An event
/// still held lies in its own's span, so its id stays; one whose own went
/// stays while another's span holds its time, a stream's `keep` at most.
const FORGET_IDS: &str = "delete from event_ids where stream = ?1 and at between ?2 and ?3
    and not exists (select 1 from heads h
        where h.stream = ?1 and h.first_at <= event_ids.at and h.last_at >= event_ids.at)
    and not exists (select 1 from segments s
        where s.stream = ?1 and s.holder is null and s.first_at <= event_ids.at and s.last_at >= event_ids.at)";
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

/// What a pass gives back at most of the pages the file holds free: a page
/// given back writes a few, so a pass's commit stays a few MiB.
const GIVE_BACK: usize = 4 << 20;

impl Records {
    /// One pass, and one at a time: give back the pages that stayed free a
    /// while, which the appends meanwhile did not need, then expire, pack
    /// and merge.
    pub(crate) fn maintain(&self) -> Result<Maintenance> {
        let _one = self.maintaining.lock().unwrap_or_else(PoisonError::into_inner);
        let given_back = self.file.give_back(GIVE_BACK, self.clock().now());
        let done = self.expire_pack_and_merge()?;
        given_back.map(|_| done)
    }

    /// Expires, packs what is ready, merges the small segments of the streams
    /// packed, and lets the appends' zstd context go, which the next append
    /// makes again.
    pub(super) fn expire_pack_and_merge(&self) -> Result<Maintenance> {
        let now = self.now();
        let expired = self.expire(now)?;
        let packing = self.pack(now)?;
        let merged = self.merge(packing.streams)?;
        self.codec.lock().unwrap_or_else(PoisonError::into_inner).rest();
        self.probe.count("tinystore_records_segments_packed_total", &[], packing.segments as f64);
        self.probe.count("tinystore_records_segments_merged_total", &[], merged as f64);
        self.probe.count("tinystore_records_expired_total", &[], expired as f64);
        // a head that failed is said once the others were packed and merged
        packing.failed.map_or(Ok(()), Err)?;
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
        let forgotten = now.saturating_sub(nanos(EXPIRED_KEPT));
        self.file.write(0, move |tx| {
            past.iter().try_for_each(|(stream, cutoff, _)| {
                execute(tx, NOTE_EXPIRED, params![stream, cutoff, now], "note what a stream keeps no longer")?;
                drop_past(tx, *stream, *cutoff, now)
            })?;
            execute(tx, FORGET_EXPIRED, [forgotten], "forget what was removed long ago").map(drop)
        })?;
        Ok(expired)
    }
}

/// Removes a stream's head rows and segments whose newest event is before
/// `cutoff`, a segment's blocks and places with it, and takes the rows from
/// what their heads hold; the events of the rows are noted for the polls.
/// An id goes only once no event its stream still holds can be its event, so
/// that a poll, which reads a segment whole, never finds an event whose id
/// was let go.
fn drop_past(tx: &Connection, stream: i64, cutoff: i64, now: i64) -> Result<()> {
    let left = tx.prepare_cached(EXPIRED_HEADS).and_then(|mut select| {
        let heads = select.query_map(params![stream, cutoff], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)));
        heads.and_then(Iterator::collect::<rusqlite::Result<Vec<(bool, i64, i64)>>>)
    });
    let left = left.map_err(|error| sql_error("count the head rows a stream keeps no longer", error))?;
    note_expired_heads(tx, stream, left.iter().map(|(_, events, _)| events).sum(), now)?;
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
    forget_ids(tx, stream, (i64::MIN, cutoff))?;
    left.into_iter().try_for_each(|(late, events, weight)| settle(tx, (stream, late), events, weight).map(drop))
}

/// Forgets the ids of a stream's events between `from` and `to` that no head
/// row or segment the stream still holds can hold: what expiry or a drop of
/// damage removed. An append of one writes its event again.
pub(super) fn forget_ids(tx: &Connection, stream: i64, (from, to): (i64, i64)) -> Result<()> {
    execute(tx, FORGET_IDS, params![stream, from, to], "forget the ids of events a stream holds no longer").map(drop)
}

/// Notes the events of head rows keep removed under a segment's id that
/// only grows, as a packing would have given them.
fn note_expired_heads(tx: &Connection, stream: i64, events: i64, now: i64) -> Result<()> {
    if events == 0 {
        return Ok(());
    }
    let taken =
        tx.prepare_cached(TAKE_ID).and_then(|mut take| take.query_row(params![stream, events], |row| row.get(0)));
    let id: i64 = taken.map_err(|error| sql_error("take a segment's id for expired head rows", error))?;
    execute(tx, GIVE_UP_ID, [id], "leave a segment's id to expired head rows")?;
    execute(tx, NOTE_EXPIRED_HEADS, params![id, events, now], "note the head rows a stream keeps no longer").map(drop)
}

#[cfg(test)]
#[path = "maintain_tests.rs"]
mod tests;
