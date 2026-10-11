//! Packing: what may no longer change of a series' head becomes blocks.
//!
//! ```text
//! the ready series, a pass's worth ─► each one's head read in a snapshot ─► its complete units cut into blocks
//!   ─► encoded outside the writer ─► published eight series a transaction, each in a savepoint of its own
//! ```
//!
//! What may be packed is a prefix: the samples before the newest time the
//! series has seen, in units of its ladder that end at or before it, so that
//! a block never crosses its unit and no later sample can enter one; a unit
//! holding more than 240 samples is cut every 240. A quiet series is never
//! forced into a block, and its head may expire whole.

use std::sync::atomic::Ordering;

use rusqlite::{Connection, OptionalExtension, params};

use super::clock::{self, MOST_BLOCKS, Times};
use super::codec::{MOST_SAMPLES, Point};
use super::directory::{Block, Group, INLINE, MOST_ENTRIES, entry_len};
use super::engine::{Engine, execute};
use super::head::{self, Extent};
use super::ladder::{unit_end, unit_of, unit_start};
use super::names::{KEEP, millis};
use super::retention::PASS_SERIES;
use super::series::Kind;
use super::summary::summarize;
use super::values;
use crate::compress::frame::Zstd;
use crate::sqlite::sql_error;
use crate::{Error, ErrorKind, Result};

/// A publication's transaction carries at most this many series and bytes.
const BATCH_SERIES: usize = 8;
const BATCH_BYTES: usize = 1 << 20;
/// What an entry may add once its payload's id is given: the longest varint.
const PAYLOAD_ID: usize = 9;

/// A series' head as packing read it: its version, which publication checks
/// nothing moved since, and the prefix it packs.
#[derive(Debug)]
pub(super) struct Candidate {
    pub(super) id: i64,
    pub(super) version: i64,
    pub(super) keep: i64,
    /// The hint of the grid's scale its next block tries.
    pub(super) scale: i64,
    pub(super) kind: Kind,
    /// The samples of its head before its watermark and within what its
    /// name keeps.
    pub(super) points: Vec<Point>,
}

/// A series' blocks, encoded and waiting for their publication.
#[derive(Debug)]
pub(super) struct Staged {
    pub(super) candidate: Candidate,
    pub(super) group: Group,
    /// The hint of the grid's scale its next block tries.
    pub(super) scale: i64,
    /// Complete units it holds past what a group takes, for the next pass.
    left: bool,
}

impl Staged {
    fn bytes(&self) -> usize {
        16 * self.candidate.points.len() + self.group.blocks.iter().map(|block| block.body.len() + 32).sum::<usize>()
    }
}

/// What packing did.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Packed {
    pub(super) samples: u64,
    pub(super) blocks: u64,
    pub(super) conflicts: u64,
    pub(super) suspended: u64,
}

const READY_AFTER: &str = "select series_id from series_state where ready = 1 and failed_at is null
    and series_id > ?1 order by series_id limit cast(?2 as integer)";
const READY_UP_TO: &str = "select series_id from series_state where ready = 1 and failed_at is null
    and series_id <= ?1 order by series_id limit cast(?2 as integer)";
const CANDIDATE: &str = "select st.version, st.max_seen, st.scale, st.head_count, st.head_start, st.head_end,
    h.body, n.kind, n.name
    from series_state st join series s on s.id = st.series_id join names n on n.id = s.name
    left join heads h on h.series_id = st.series_id
    where st.series_id = ?1 and st.failed_at is null";
const UNREADY: &str = "update series_state set ready = 0 where series_id = ?1 and version = ?2";
const SUSPEND: &str = "update series_state set failed_at = ?2, failure = ?3 where series_id = ?1 and failed_at is null";

impl Engine {
    /// Packs a pass's worth of the ready series, a group each, and says
    /// whether more wait: other series, or more complete units of one than a
    /// group takes.
    pub(super) fn pack(&self, now: i64) -> Result<(Packed, bool)> {
        let ready = self.ready_series()?;
        let mut unfinished = ready.len() == PASS_SERIES;
        let mut packed = Packed::default();
        let mut staged: Vec<Staged> = Vec::new();
        let mut zstd = self.codecs.take();
        for id in ready {
            let Some(series) = self.stage(&mut zstd, id, now, &mut packed)? else { continue };
            unfinished |= series.left;
            let full = staged.len() == BATCH_SERIES
                || staged.iter().map(Staged::bytes).sum::<usize>() + series.bytes() > BATCH_BYTES;
            if full {
                self.publish(std::mem::take(&mut staged), &mut packed)?;
            }
            staged.push(series);
        }
        self.publish(staged, &mut packed)?;
        self.codecs.give_back(zstd);
        Ok((packed, unfinished))
    }

    /// The ready series of a pass, after the last a pass took and around,
    /// so that the series early in the table never starve the rest.
    fn ready_series(&self) -> Result<Vec<i64>> {
        let after = self.pack_cursor.load(Ordering::Relaxed);
        let failed = |error| sql_error("find the series ready to pack", error);
        let ids = self.file.read(|connection| {
            let mut ids = ids_of(connection, READY_AFTER, after, PASS_SERIES).map_err(failed)?;
            if ids.len() < PASS_SERIES && after > 0 {
                ids.extend(ids_of(connection, READY_UP_TO, after, PASS_SERIES - ids.len()).map_err(failed)?);
            }
            Ok(ids)
        })?;
        if let Some(last) = ids.last() {
            self.pack_cursor.store(*last, Ordering::Relaxed);
        }
        Ok(ids)
    }

    /// One series read and encoded as blocks; none when it holds no complete
    /// unit, or failed on its own bytes and was suspended.
    fn stage(&self, zstd: &mut Zstd, id: i64, now: i64, packed: &mut Packed) -> Result<Option<Staged>> {
        let staged = self.candidate(zstd, id, now).and_then(|candidate| match candidate {
            Some(candidate) if !candidate.points.is_empty() => encode(zstd, candidate).map(Some),
            Some(candidate) => self.unready(&candidate).map(|()| None),
            None => Ok(None),
        });
        match staged {
            Err(error) if matches!(error.kind(), ErrorKind::Corrupt | ErrorKind::Limit) => {
                packed.suspended += self.suspend(id, now, &error)?;
                Ok(None)
            }
            staged => staged,
        }
    }

    /// A series' head as one snapshot holds it, cut to its complete units.
    fn candidate(&self, zstd: &mut Zstd, id: i64, now: i64) -> Result<Option<Candidate>> {
        let row = self.file.read(|connection| read_candidate(connection, id))?;
        let Some(row) = row else { return Ok(None) };
        let keep = millis(self.names.get(&row.name).map_or(KEEP, |name| name.keeps()));
        let mut points = Vec::new();
        if let Some(bytes) = &row.head {
            let chunks = head::parse(id, row.extent, bytes)?;
            head::decode(zstd, &chunks, i64::MIN, i64::MAX, &mut points)?;
        }
        let cutoff = now.saturating_sub(keep);
        let kind = Kind::parse(&row.kind).ok_or_else(|| Error::corrupt("a series whose name has no kind"))?;
        let points = complete_units(&points, cutoff, row.max_seen);
        Ok(Some(Candidate { id, version: row.version, keep, scale: row.scale, kind, points }))
    }

    /// Clears a series' ready flag when nothing has moved since it was read:
    /// its head holds no complete unit yet.
    fn unready(&self, candidate: &Candidate) -> Result<()> {
        let (id, version) = (candidate.id, candidate.version);
        self.file.write(0, move |tx| execute(tx, UNREADY, params![id, version], "wait for a block's worth").map(drop))
    }

    /// Stops a series taking samples, with what broke, said once; its
    /// neighbours go on.
    pub(super) fn suspend(&self, id: i64, now: i64, error: &Error) -> Result<u64> {
        let failure: String = error.to_string().chars().take(1024).collect();
        let reason = failure.clone();
        let changed =
            self.file.write(0, move |tx| execute(tx, SUSPEND, params![id, now, reason], "suspend a series"))?;
        if changed == 1 {
            tracing::warn!(target: "tinystore", series = id, failure, "metrics: a series stopped taking samples");
        }
        Ok(changed as u64)
    }
}

/// A candidate's row.
struct CandidateRow {
    version: i64,
    max_seen: i64,
    scale: i64,
    extent: Extent,
    head: Option<Vec<u8>>,
    kind: String,
    name: String,
}

fn read_candidate(connection: &Connection, id: i64) -> Result<Option<CandidateRow>> {
    let row = connection.prepare_cached(CANDIDATE).and_then(|mut select| {
        select
            .query_row(params![id], |row| {
                let count: i64 = row.get(3)?;
                Ok(CandidateRow {
                    version: row.get(0)?,
                    max_seen: row.get(1)?,
                    scale: row.get(2)?,
                    extent: Extent {
                        count: usize::try_from(count).unwrap_or(0),
                        start: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        end: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    },
                    head: row.get(6)?,
                    kind: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                    name: row.get(8)?,
                })
            })
            .optional()
    });
    row.map_err(|error| sql_error("read a series to pack", error))
}

fn ids_of(connection: &Connection, sql: &str, cursor: i64, most: usize) -> rusqlite::Result<Vec<i64>> {
    let mut select = connection.prepare_cached(sql)?;
    let ids = select.query_map(params![cursor, most as i64], |row| row.get(0))?;
    ids.collect()
}

/// The samples within what a name keeps, before the watermark, in the units
/// of the ladder that end at or before it.
fn complete_units(points: &[Point], cutoff: i64, watermark: i64) -> Vec<Point> {
    let kept: Vec<Point> = points.iter().copied().filter(|point| point.at >= cutoff && point.at < watermark).collect();
    let (Some(first), Some(last)) = (kept.first(), kept.last()) else { return Vec::new() };
    let unit = unit_of(kept.len(), first.at, last.at);
    let complete = kept.partition_point(|point| unit_end(point.at, unit) <= watermark);
    kept[..complete].to_vec()
}

/// A candidate's prefix as a group's blocks, cut where its units begin, each
/// of 240 samples at most, as many as a group holds: 32 blocks, a clock of
/// 64 KiB and a directory of 8 KiB.
fn encode(zstd: &mut Zstd, mut candidate: Candidate) -> Result<Staged> {
    let unit = unit_of(candidate.points.len(), candidate.points[0].at, candidate.points[candidate.points.len() - 1].at);
    let mut blocks: Vec<Block> = Vec::new();
    let (mut clock_bytes, mut entry_bytes, mut taken) = (6, 0, 0);
    let mut scale = candidate.scale;
    for run in cut(&candidate.points, unit) {
        if blocks.len() == MOST_BLOCKS {
            break;
        }
        let times = Times::of(run);
        if !blocks.is_empty() && clock_bytes + times.steps.len() + 32 > clock::MOST_BYTES {
            break;
        }
        let (body, hint) = values::encode(zstd, run, &times, scale)?;
        let body = body.unwrap_or_default();
        let block = Block {
            first: run[0].value,
            summary: summarize(run, candidate.kind),
            body_len: body.len(),
            payload: 0,
            body,
            times,
        };
        // its payload's id is given at publication: room for the longest
        let entry = entry_len(&block) + if block.body_len > INLINE { PAYLOAD_ID } else { 0 };
        if !blocks.is_empty() && entry_bytes + entry > MOST_ENTRIES {
            break;
        }
        (clock_bytes, entry_bytes, taken, scale) =
            (clock_bytes + block.times.steps.len() + 32, entry_bytes + entry, taken + run.len(), hint);
        blocks.push(block);
    }
    let left = taken < candidate.points.len();
    candidate.points.truncate(taken);
    let group = Group {
        series: candidate.id,
        start: blocks[0].times.start,
        end: blocks[blocks.len() - 1].times.end,
        clock: 0,
        live: mask(blocks.len()),
        blocks,
    };
    Ok(Staged { candidate, group, scale, left })
}

/// The runs of a prefix that blocks take: a unit's samples, every 240 when
/// it holds more.
fn cut(points: &[Point], unit: i64) -> impl Iterator<Item = &[Point]> {
    points
        .chunk_by(move |a, b| unit_start(a.at, unit) == unit_start(b.at, unit))
        .flat_map(|units| units.chunks(MOST_SAMPLES))
}

/// The mask of a group's first `count` slots.
pub(super) fn mask(count: usize) -> u32 {
    if count >= 32 { u32::MAX } else { (1 << count) - 1 }
}

#[cfg(test)]
#[path = "pack_tests.rs"]
mod tests;
