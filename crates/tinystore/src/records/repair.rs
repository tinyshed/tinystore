//! What no longer reads: a head row or a segment whose bytes changed, met by
//! a read, a packing, a merge or a poll. The engine keeps what it met while it
//! runs and says each once; `damaged` lists it with its stream and its
//! events' times, and `drop` removes it, so that the reads over it work
//! again. A packing and a merge go past it meanwhile, and what still reads is
//! never dropped.
//!
//! ```text
//! a read     events audit: segment 3 of audit: block 7: a block's checksum: corrupt
//! damaged    segment 3 of audit, its events from 12:00:01 to 12:59:58
//! drop       segment 3 gone with its blocks and places, a poll behind it told they expired
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{OptionalExtension, params};

use super::block;
use super::engine::{Records, execute};
use super::events::AllEvents;
use super::head::{self, Codec};
use super::pack::{HELD, PATIENCE, settle};
use super::segment::Schema;
use crate::sqlite::sql_error;
use crate::{Error, ErrorKind, Result};

/// What holds a damage's events: a head row those not packed yet, a segment
/// packed ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Part {
    HeadRow,
    Segment,
}

impl fmt::Display for Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Part::HeadRow => "head row",
            Part::Segment => "segment",
        })
    }
}

/// A head row or a segment that no longer reads, as `damaged` lists it and
/// `drop` takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Damage {
    pub part: Part,
    pub id: i64,
    pub stream: String,
    /// Its events' oldest and newest times.
    pub first: SystemTime,
    pub last: SystemTime,
    /// What broke, as the read that met it said.
    pub error: String,
}

impl fmt::Display for Damage {
    /// As a read's error names it: `segment 3 of audit`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} of {}", self.part, self.id, self.stream)
    }
}

/// What the engine met that no longer reads, by part and id: its stream and
/// what broke.
pub(super) type Damaged = BTreeMap<(Part, i64), (String, String)>;

/// A segment's row and what its events weigh, and its blocks' bodies.
type SegmentBytes = ((Vec<u8>, i64), Vec<Vec<u8>>);

const HEAD_TIMES: &str = "select first_at, last_at from heads where id = ?1";
const SEGMENT_TIMES: &str = "select first_at, last_at from segments where id = ?1 and holder is null";
const HEAD_BODY: &str = "select body from heads where id = ?1";
const HEAD_COUNTS: &str = "select stream, late, count, weight from heads where id = ?1";
const DROP_HEAD_ROW: &str = "delete from heads where id = ?1";
const SEGMENT_BODY: &str = "select body, weight from segments where id = ?1 and holder is null";
const SEGMENT_BLOCKS: &str = "select body from blocks where segment = ?1 order by id";
/// What a segment dropped held, its places' among them, for the polls
/// behind it, as expiry notes what it removes.
const NOTE_DROPPED: &str = "insert or replace into expired (segment, events, at)
    select id, count, ?2 from segments where id = ?1 or holder = ?1";
const DROP_SEGMENT: [(&str, &str); 6] = [
    ("delete from block_traces where block in (select id from blocks where segment = ?1)", "blocks' traces"),
    ("delete from block_filters where block in (select id from blocks where segment = ?1)", "blocks' filters"),
    ("delete from segment_keys where segment = ?1", "keys"),
    ("delete from blocks where segment = ?1", "blocks"),
    ("delete from segments where holder = ?1", "places"),
    ("delete from segments where id = ?1", "row"),
];

impl AllEvents {
    /// What reads, packing, merges and polls met that no longer reads,
    /// which `drop` removes: each head row and segment with its stream and
    /// its events' times. What this run of the store has not met yet is not
    /// listed.
    pub fn damaged(&self) -> Result<Vec<Damage>> {
        self.records.damaged().map_err(|error| error.within("events"))
    }

    /// Removes a head row or a segment that no longer reads, so that the
    /// reads over it work again: a segment with its blocks and places, its
    /// events counted as expired for a poll behind it. What still reads is
    /// refused.
    pub fn drop(&self, damage: &Damage) -> Result<()> {
        self.records.drop_damage(damage).map_err(|error| error.within(format!("events: {damage}")))
    }
}

impl Records {
    /// Names an error of a part that did not read as a drop names the part,
    /// and keeps the part when its bytes changed, said once a run.
    pub(super) fn met(&self, (part, id): (Part, i64), stream: &str, error: Error) -> Error {
        if error.kind() == ErrorKind::Corrupt {
            let met = (stream.to_owned(), error.to_string());
            if self.lock_damaged().insert((part, id), met).is_none() {
                tracing::warn!(
                    target: "tinystore",
                    "records: {part} {id} of {stream} no longer reads, and reads over it fail till it is dropped: {error}"
                );
            }
        }
        error.within(format!("{part} {id} of {stream}"))
    }

    pub(super) fn is_damaged(&self, part: Part, id: i64) -> bool {
        self.lock_damaged().contains_key(&(part, id))
    }

    /// A stream's name by its id, the id itself when no stream has it.
    pub(super) fn name_of(&self, stream: i64) -> String {
        let known = self.every_stream().into_iter().find(|known| known.id == stream);
        known.map_or_else(|| stream.to_string(), |known| known.name.to_string())
    }

    /// What was met, with its times read from the file; what left the file
    /// since, expired or dropped, is forgotten.
    fn damaged(&self) -> Result<Vec<Damage>> {
        let met: Vec<((Part, i64), (String, String))> =
            self.lock_damaged().iter().map(|(key, met)| (*key, met.clone())).collect();
        let failed = |error| sql_error("read the times of what no longer reads", error);
        let times: Vec<Option<(i64, i64)>> = self.file.read(|connection| {
            let mut timed = Vec::with_capacity(met.len());
            for ((part, id), _) in &met {
                let sql = if *part == Part::HeadRow { HEAD_TIMES } else { SEGMENT_TIMES };
                let mut select = connection.prepare_cached(sql).map_err(failed)?;
                let read = select.query_row([id], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(failed)?;
                timed.push(
                    read.map(|(first, last): (Option<i64>, Option<i64>)| (first.unwrap_or(0), last.unwrap_or(0))),
                );
            }
            Ok(timed)
        })?;
        let mut damages = Vec::new();
        for (((part, id), (stream, error)), times) in met.into_iter().zip(times) {
            match times {
                Some((first, last)) => {
                    damages.push(Damage { part, id, stream, first: time_of(first), last: time_of(last), error });
                }
                None => drop(self.lock_damaged().remove(&(part, id))),
            }
        }
        Ok(damages)
    }

    /// Removes a part that no longer reads, under maintenance's lock, so
    /// that no packing takes the row and no merge the segment meanwhile.
    fn drop_damage(&self, damage: &Damage) -> Result<()> {
        let _one = self.maintaining.lock().unwrap_or_else(PoisonError::into_inner);
        if self.still_reads(damage)? {
            return Err(Error::invalid("it still reads, and only what no longer reads is dropped"));
        }
        match damage.part {
            Part::HeadRow => self.drop_head_row(damage.id)?,
            Part::Segment => self.drop_segment(damage.id)?,
        }
        self.lock_damaged().remove(&(damage.part, damage.id));
        Ok(())
    }

    /// Whether every byte of a part reads still; one that left the file has
    /// nothing left to read.
    fn still_reads(&self, damage: &Damage) -> Result<bool> {
        let failed = |error| sql_error("read what a drop removes", error);
        let mut codec = Codec::default();
        if damage.part == Part::HeadRow {
            let body: Option<Vec<u8>> = self.file.read(|connection| {
                let mut select = connection.prepare_cached(HEAD_BODY).map_err(failed)?;
                select.query_row([damage.id], |row| row.get(0)).optional().map_err(failed)
            })?;
            return Ok(body.is_some_and(|body| head::parse(&body, &mut codec).is_ok()));
        }
        let read: Option<SegmentBytes> = self.file.read(|connection| {
            let mut select = connection.prepare_cached(SEGMENT_BODY).map_err(failed)?;
            let row = select.query_row([damage.id], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(failed)?;
            let Some(row) = row else { return Ok(None) };
            let mut blocks = connection.prepare_cached(SEGMENT_BLOCKS).map_err(failed)?;
            let bodies = blocks.query_map([damage.id], |row| row.get(0)).and_then(Iterator::collect);
            Ok(Some((row, bodies.map_err(failed)?)))
        })?;
        let Some(((body, weight), blocks)) = read else { return Ok(false) };
        let _held = self.memory.reserve(weight.max(0) as u64 * HELD, "records: reading a segment to drop", PATIENCE)?;
        let schema = Schema::parse(&body, &mut codec);
        let reads = schema
            .and_then(|schema| blocks.iter().try_for_each(|block| block::parse(&schema, block, &mut codec).map(drop)));
        Ok(reads.is_ok())
    }

    /// Removes a head row and takes it from what its head holds.
    fn drop_head_row(&self, id: i64) -> Result<()> {
        let emptied = self.file.write(0, move |tx| {
            let mut select = tx.prepare_cached(HEAD_COUNTS).map_err(|error| sql_error("read a head row", error))?;
            let counts = select.query_row([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)));
            let counts: Option<(i64, bool, i64, i64)> =
                counts.optional().map_err(|error| sql_error("read a head row", error))?;
            let Some((stream, late, events, weight)) = counts else { return Ok(None) };
            execute(tx, DROP_HEAD_ROW, [id], "remove a head row that no longer reads")?;
            settle(tx, (stream, late), events, weight).map(|emptied| (emptied && !late).then_some(stream))
        })?;
        if let Some(stream) = emptied {
            self.waiting.lock().unwrap_or_else(PoisonError::into_inner).remove(&stream);
        }
        Ok(())
    }

    /// Removes a segment with its blocks and places, noting what they held.
    fn drop_segment(&self, id: i64) -> Result<()> {
        let now = self.now();
        self.file.write(0, move |tx| {
            execute(tx, NOTE_DROPPED, params![id, now], "note what a dropped segment held")?;
            DROP_SEGMENT.iter().try_for_each(|(sql, what)| {
                execute(tx, sql, [id], &format!("remove a dropped segment's {what}")).map(drop)
            })
        })
    }

    fn lock_damaged(&self) -> MutexGuard<'_, Damaged> {
        self.damaged.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn time_of(nanos: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(nanos.max(0) as u64)
}

#[cfg(test)]
#[path = "repair_tests.rs"]
mod tests;
