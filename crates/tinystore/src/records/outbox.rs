//! A database's streams: the events a transaction or a batch of an
//! application's database appends to a stream, kept in the database's file
//! beside its rows until they are moved into records.db, so that an event
//! exists exactly when the change it records does.
//!
//! ```text
//! db.tx: rows + outbox rows ── commit ── move: one append a stream, ids kept ── outbox rows removed
//!                                     └── a crash here: moved when db.events opens, or by maintenance
//! ```
//!
//! Every event the outbox keeps has an id, the program's or one the store
//! gives, and records.db holds the ids of the events its streams keep: a move
//! made twice, after a crash between an append and the removal, writes each
//! event once.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use rusqlite::params;

use super::engine::{Records, Stream, execute};
use super::event::{Fields, Kept, Level, SpanId, TraceId};
use super::events::window;
use crate::engine::{Engine, SharedFile};
use crate::sqlite::{File, Migration, Tx, query_cached, sql_error};
use crate::{Error, Result};

/// records' table in a database's file, a history of its own there.
const IN_A_DATABASE: &[Migration] =
    &[Migration::of(1, "0001_in_a_database.sql", include_str!("migrations/0001_in_a_database.sql"))];

/// The events one read of a move takes: a move of more reads again.
const MOVE_ROWS: usize = 1024;

const KEEP: &str = "insert into _tinystore_records_outbox
    (stream, id, at, name, level, message, trace_id, span_id, context, fields)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";
const KEPT_ID: &str = "select 1 from _tinystore_records_outbox where stream = ?1 and id = ?2";
const KEPT_AFTER: &str = "select seq, stream, id, at, name, level, message, trace_id, span_id, context, fields
    from _tinystore_records_outbox where seq > ?1 order by seq limit cast(?2 as integer)";
const REMOVE: &str = "delete from _tinystore_records_outbox where seq = ?1";

/// The events a database's transactions appended to the store's streams,
/// kept in its file until each is moved into its stream.
pub(crate) struct Outbox {
    file: Arc<File>,
    /// What an error calls the file: `sql app`.
    owner: String,
    records: Arc<Records>,
    /// Held by a move, so that two never read the same events.
    moving: Mutex<()>,
    /// Whether the last move failed: a failure is logged once, and its
    /// recovery once.
    failing: AtomicBool,
}

impl Engine for Outbox {
    /// Moves nothing: what a failed move left goes at the next open.
    fn close(&self) -> Result<()> {
        Ok(())
    }
}

/// An event the outbox holds: its place in the order of appends, and its
/// stream's name.
struct Left {
    seq: i64,
    stream: String,
    event: Kept,
}

impl Outbox {
    /// The outbox kept in a database's file, its table made the first time,
    /// which records' maintenance moves from as it does its own work.
    pub(super) fn in_file(shared: &SharedFile, records: &Arc<Records>) -> Result<Arc<Outbox>> {
        shared.engine(|store| {
            #[cfg(feature = "replication")]
            if store.node().is_some() {
                let why = "a database's events on a node, whose copies move no events yet";
                return Err(Error::invalid(format!("{}: {why}", shared.owner())));
            }
            let _ = store;
            shared.file().migrate("records", IN_A_DATABASE)?;
            let outbox = Arc::new(Outbox {
                file: Arc::clone(shared.file()),
                owner: shared.owner().to_owned(),
                records: Arc::clone(records),
                moving: Mutex::default(),
                failing: AtomicBool::new(false),
            });
            records.keep_outbox(&outbox);
            Ok(outbox)
        })
    }

    pub(super) fn file(&self) -> &Arc<File> {
        &self.file
    }

    /// The file as an error names it: `sql app`.
    pub(super) fn place(&self) -> &str {
        &self.owner
    }

    /// Keeps `event` of `stream` in `tx`, a transaction of the file, given an
    /// id unless it has one; says whether its move will write it: `false`
    /// when its stream holds its id, or an event kept before it does.
    pub(super) fn keep(&self, tx: &Tx<'_>, stream: &Stream, mut event: Kept) -> Result<bool> {
        match &event.id {
            Some(id) if self.records.holds_id(stream, id)? || self.kept_id(tx, stream, id)? => return Ok(false),
            Some(_) => {}
            None => event.id = Some(fresh_id(event.at)),
        }
        let level = event.level.map(Level::number);
        let (trace, span) = (event.trace_id.map(|id| id.0.to_vec()), event.span_id.map(|id| id.0.to_vec()));
        let (context, fields) = (event.context.object(), event.fields.object());
        let row =
            params![&*stream.name, event.id, event.at, event.name, level, event.message, trace, span, context, fields];
        execute(tx, KEEP, row, "keep an event for its stream").map(|_| true)
    }

    fn kept_id(&self, tx: &Tx<'_>, stream: &Stream, id: &str) -> Result<bool> {
        let found = tx.prepare_cached(KEPT_ID).and_then(|mut select| select.exists(params![&*stream.name, id]));
        found.map_err(|error| sql_error("look up a kept event's id", error))
    }

    /// Moves what the outbox holds, as [`Outbox::moved`] does, and says a
    /// failure once in the log: the events stay for the next move.
    pub(super) fn move_left(&self) {
        match self.moved() {
            Ok(_) if self.failing.swap(false, Ordering::SeqCst) => {
                tracing::info!(target: "tinystore", database = %self.owner, "records: a database's events moved again");
            }
            Ok(_) => {}
            Err(error) if !self.failing.swap(true, Ordering::SeqCst) => {
                tracing::warn!(
                    target: "tinystore",
                    database = %self.owner,
                    %error,
                    "records: a database's committed events, not moved into their streams yet",
                );
            }
            Err(_) => {}
        }
    }

    /// Moves every event the outbox holds into its stream, in the order they
    /// were appended, then removes it; says how many it moved. One its stream
    /// keeps no longer is dropped, as retention would have dropped it, and
    /// one ahead of the store's clock, which went back, stays for a later
    /// move.
    fn moved(&self) -> Result<usize> {
        let _one = self.moving.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut after, mut moved) = (0, 0);
        loop {
            let left = self.left_after(after)?;
            let Some(last) = left.last() else { break };
            after = last.seq;
            let full = left.len() == MOVE_ROWS;
            for (stream, events) in by_stream(left) {
                moved += self.move_stream(&stream, events).map_err(|error| error.within(format!("events {stream}")))?;
            }
            if !full {
                break;
            }
        }
        Ok(moved)
    }

    /// The events kept after `seq`, a move's read of them.
    fn left_after(&self, seq: i64) -> Result<Vec<Left>> {
        let asked = (KEPT_AFTER, "read the kept events");
        let read = self.file.read(|connection| query_cached(connection, asked, params![seq, MOVE_ROWS as i64], raw));
        let rows: Vec<Raw> = read.map_err(|error| error.within(&self.owner))?;
        rows.into_iter().map(Raw::left).collect::<Result<_>>().map_err(|error| error.within(&self.owner))
    }

    /// Appends one stream's events, those it keeps no longer dropped and
    /// those ahead of the clock left, then removes what it took.
    fn move_stream(&self, name: &str, events: Vec<Left>) -> Result<usize> {
        let stream = self.records.open_stream(name, None)?;
        let (oldest, newest) = window(&stream, self.records.now());
        let (mut taken, mut moving, mut dropped) = (Vec::new(), Vec::new(), 0);
        for Left { seq, event, .. } in events {
            if event.at > newest {
                continue;
            }
            taken.push(seq);
            if event.at < oldest {
                dropped += 1;
            } else {
                moving.push(event);
            }
        }
        let moved = moving.len();
        self.records.append_kept(&stream, moving)?;
        if dropped > 0 {
            tracing::warn!(
                target: "tinystore",
                stream = name,
                database = %self.owner,
                dropped,
                "records: a database's committed events its stream keeps no longer by their move, dropped",
            );
        }
        self.remove(taken)?;
        Ok(moved)
    }

    /// Removes the events of `seqs` from the outbox, in one commit.
    fn remove(&self, seqs: Vec<i64>) -> Result<()> {
        if seqs.is_empty() {
            return Ok(());
        }
        let removed = self.file.write(seqs.len() * 8, move |tx| {
            seqs.iter().try_for_each(|seq| execute(tx, REMOVE, params![seq], "remove a moved event").map(drop))
        });
        removed.map_err(|error| error.within(&self.owner))
    }
}

impl Records {
    /// Keeps `outbox` for maintenance, which moves what it holds.
    fn keep_outbox(&self, outbox: &Arc<Outbox>) {
        let mut outboxes = self.outboxes.lock().unwrap_or_else(PoisonError::into_inner);
        outboxes.retain(|kept| kept.strong_count() > 0);
        outboxes.push(Arc::downgrade(outbox));
    }

    /// Moves what each database's outbox holds: events a crash or a failed
    /// move left, each failure said once in the log.
    pub(super) fn move_outboxes(&self) {
        let outboxes: Vec<Arc<Outbox>> =
            self.outboxes.lock().unwrap_or_else(PoisonError::into_inner).iter().filter_map(Weak::upgrade).collect();
        for outbox in outboxes {
            outbox.move_left();
        }
    }
}

/// A row of the outbox as SQLite gives it.
struct Raw {
    seq: i64,
    stream: String,
    id: String,
    at: i64,
    name: String,
    level: Option<i64>,
    message: Option<String>,
    trace: Option<Vec<u8>>,
    span: Option<Vec<u8>>,
    context: String,
    fields: String,
}

fn raw(row: &rusqlite::Row<'_>) -> rusqlite::Result<Raw> {
    Ok(Raw {
        seq: row.get(0)?,
        stream: row.get(1)?,
        id: row.get(2)?,
        at: row.get(3)?,
        name: row.get(4)?,
        level: row.get(5)?,
        message: row.get(6)?,
        trace: row.get(7)?,
        span: row.get(8)?,
        context: row.get(9)?,
        fields: row.get(10)?,
    })
}

impl Raw {
    /// The event the row keeps: a value no append could have kept is
    /// damage, named by what broke.
    fn left(self) -> Result<Left> {
        let level = self.level.map(|number| i32::try_from(number).map(Level::of)).transpose();
        let level = level.map_err(|_| Error::corrupt("a kept event's level out of range"))?;
        let trace_id = self.trace.map(|bytes| <[u8; 16]>::try_from(bytes).map(TraceId)).transpose();
        let trace_id = trace_id.map_err(|_| Error::corrupt("a kept event's trace id that is not 16 bytes"))?;
        let span_id = self.span.map(|bytes| <[u8; 8]>::try_from(bytes).map(SpanId)).transpose();
        let span_id = span_id.map_err(|_| Error::corrupt("a kept event's span id that is not 8 bytes"))?;
        let read =
            |json: &str| Fields::read(json).map_err(|_| Error::corrupt("a kept event's fields that are no object"));
        let (context, fields) = (read(&self.context)?, read(&self.fields)?);
        let (at, id, name, message) = (self.at, Some(self.id), self.name, self.message);
        let event = Kept { at, id, name, level, message, fields, context, trace_id, span_id };
        Ok(Left { seq: self.seq, stream: self.stream, event })
    }
}

/// Events parted by their stream, each stream's in the order they came.
fn by_stream(left: Vec<Left>) -> Vec<(String, Vec<Left>)> {
    let mut streams: Vec<(String, Vec<Left>)> = Vec::new();
    for event in left {
        match streams.iter_mut().find(|(name, _)| *name == event.stream) {
            Some((_, events)) => events.push(event),
            None => streams.push((event.stream.clone(), vec![event])),
        }
    }
    streams
}

/// An id no other event is given, as a UUID of version 7 spells one: the
/// event's time in milliseconds, then 74 bits drawn for it.
///
/// The bits are SipHash's of a count, under the keys std draws from the
/// system for each thread's first `RandomState` and steps for each after:
/// no dependency for a source of random bytes, and an id needs no secret.
fn fresh_id(at: i64) -> String {
    let millis = (at.max(0) as u64 / 1_000_000) & 0xffff_ffff_ffff;
    let high = (millis << 16) | 0x7000 | (drawn() & 0x0fff);
    let low = 0x8000_0000_0000_0000 | (drawn() & 0x3fff_ffff_ffff_ffff);
    let (time_high, time_low, version) = (high >> 32, (high >> 16) & 0xffff, high & 0xffff);
    format!("{time_high:08x}-{time_low:04x}-{version:04x}-{:04x}-{:012x}", low >> 48, low & 0xffff_ffff_ffff)
}

/// 64 bits no earlier call gave.
fn drawn() -> u64 {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(COUNT.fetch_add(1, Ordering::Relaxed));
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_id_is_a_uuid_of_version_7_of_the_events_time() {
        let at = 1_800_000_000_123_456_789;
        let id = fresh_id(at);
        assert_eq!(id.len(), 36, "{id}");
        let digits: String = id.chars().filter(|c| *c != '-').collect();
        assert_eq!(u64::from_str_radix(&digits[..12], 16).unwrap(), 1_800_000_000_123, "{id}");
        assert_eq!(&id[14..15], "7", "{id}");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"), "{id}");
        let ids: std::collections::HashSet<String> = (0..10_000).map(|_| fresh_id(at)).collect();
        assert_eq!(ids.len(), 10_000);
    }
}

#[cfg(test)]
#[path = "outbox_tests.rs"]
mod outbox_tests;
