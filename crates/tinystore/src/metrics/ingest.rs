//! `ingest`: series and their samples, written whole or not at all.
//!
//! ```text
//! check every series and gather its samples ─► settle the names ─► check each sample's time
//!     ─► one grouped write: each series found or registered, its head rewritten
//! ```
//!
//! A call is one savepoint of the file's grouped commit: a series it
//! registers, its pairs and postings, and every head it rewrites commit
//! together, or nothing does. A refused series names itself by its labels, so
//! that the call can be sent again without it.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use rusqlite::{OptionalExtension, params};

use super::codec::Point;
use super::engine::{AHEAD, Engine, execute};
use super::head::{self, Extent};
use super::labels::Kept;
use super::ladder::{unit_end, unit_of};
use super::names::{Name, checked_keep, millis};
use super::registry;
use super::retention::expires;
use super::series::{Kind, Series};
use super::timestamp::rfc3339;
use crate::compress::frame::Zstd;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, ErrorKind, Result, unix_millis};

/// What one call carries at most: more is sent in several.
const MOST_SAMPLES: usize = 10_000;
const MOST_BYTES: usize = 4 << 20;
/// What a sample weighs against the call's bytes, a time and a value.
const SAMPLE_BYTES: usize = 16;
/// How long a call waits for the store's memory.
const PATIENCE: Duration = Duration::from_secs(10);

/// One series' samples of a call, checked, in the order of their times, a
/// time given twice holding the value given last.
#[derive(Debug)]
pub(super) struct Batch {
    pub(super) kept: Kept,
    pub(super) identity: [u8; 32],
    pub(super) kind: Kind,
    /// Its name's id.
    pub(super) name: i64,
    /// How long its name keeps it, in milliseconds.
    pub(super) keep: i64,
    pub(super) points: Vec<Point>,
}

/// A series of a call as it is gathered, before its name is settled.
struct Gathered {
    kept: Kept,
    kind: Kind,
    keep: Option<Duration>,
    points: Vec<Point>,
}

impl Engine {
    /// Writes every series of the call or none of them.
    pub(super) fn ingest(&self, series: &[Series]) -> Result<()> {
        let now = self.now();
        let (gathered, bytes) = gather(series)?;
        let _held = self.memory.reserve(bytes as u64, "metrics: an ingest", PATIENCE)?;
        let names = self.names.settle(&self.file, &wanted_names(&gathered)?)?;
        let batches = gathered.into_values().map(|series| batch(series, &names, now)).collect::<Result<Vec<_>>>()?;
        if batches.is_empty() {
            return Ok(());
        }
        if self.write(batches, bytes)? {
            self.wake();
        }
        Ok(())
    }

    /// The call's one write, and whether a series of it now holds a
    /// block's worth.
    fn write(&self, batches: Vec<Batch>, bytes: usize) -> Result<bool> {
        let zstd = self.codecs.take();
        let (zstd, ready) = self.file.write(bytes, move |tx| {
            let mut zstd = zstd;
            let mut ready = false;
            for batch in &batches {
                let written = write_series(tx, &mut zstd, batch);
                ready |= written.map_err(|error| error.within(format!("metrics {}", batch.kept)))?;
            }
            Ok((zstd, ready))
        })?;
        self.codecs.give_back(zstd);
        Ok(ready)
    }
}

/// Checks each series and gathers its samples by its labels, within the
/// call's bounds: one series given in several parts is one.
fn gather(series: &[Series]) -> Result<(BTreeMap<[u8; 32], Gathered>, usize)> {
    let (mut gathered, mut samples, mut bytes) = (BTreeMap::new(), 0, 0);
    for given in series {
        let named = || format!("metrics {given}");
        let kept = Kept::of(&given.name, &given.labels).map_err(|error| error.within(named()))?;
        if given.samples.is_empty() {
            return Err(Error::invalid(format!("{}: a series without samples", named())));
        }
        let keep = given.keep.map(checked_keep).transpose().map_err(|error| error.within(named()))?;
        samples += given.samples.len();
        bytes += kept.bytes() + SAMPLE_BYTES * given.samples.len();
        if samples > MOST_SAMPLES || bytes > MOST_BYTES {
            return Err(Error::limit(format!(
                "metrics: an ingest past {MOST_SAMPLES} samples or {} MiB: send it in parts",
                MOST_BYTES >> 20
            ))
            .naming("limit", "one ingest")
            .naming("wanted", samples.to_string())
            .naming("bound", MOST_SAMPLES.to_string()));
        }
        let points = given.samples.iter().map(|sample| Point { at: unix_millis(sample.at), value: sample.value });
        let entry = gathered.entry(kept.identity()).or_insert_with(|| Gathered {
            kept: kept.clone(),
            kind: given.kind,
            keep,
            points: Vec::new(),
        });
        if entry.kind != given.kind || (keep.is_some() && entry.keep.is_some_and(|before| Some(before) != keep)) {
            return Err(Error::invalid(format!("{}: given twice in one ingest, as two kinds or two keeps", named())));
        }
        entry.keep = entry.keep.or(keep);
        entry.points.extend(points);
    }
    Ok((gathered, bytes))
}

/// Each name of the call once, with the keep it gives; two keeps of one name
/// in a call are refused.
fn wanted_names(gathered: &BTreeMap<[u8; 32], Gathered>) -> Result<BTreeMap<&str, Option<Duration>>> {
    let mut wanted: BTreeMap<&str, Option<Duration>> = BTreeMap::new();
    for series in gathered.values() {
        let given = wanted.entry(series.kept.name()).or_default();
        match (*given, series.keep) {
            (Some(before), Some(keep)) if before != keep => {
                return Err(Error::invalid(format!("metrics {}: two keeps in one ingest", series.kept.name())));
            }
            (None, keep) => *given = keep,
            _ => {}
        }
    }
    Ok(wanted)
}

/// A series' samples as its write takes them: each inside its name's window,
/// in the order of their times, a time given twice holding the later value.
fn batch(series: Gathered, names: &HashMap<String, Name>, now: i64) -> Result<Batch> {
    let named = || format!("metrics {}", series.kept);
    let name = names.get(series.kept.name()).ok_or_else(|| Error::internal(format!("{}: its name", named())))?;
    let keep = millis(name.keeps());
    let (cutoff, horizon) = (now.saturating_sub(keep), now.saturating_add(millis(AHEAD)));
    for point in &series.points {
        let refused = |kind, why| Error::new(kind, format!("{}: sample at {}{why}", named(), rfc3339(point.at)));
        if point.at < cutoff {
            return Err(refused(ErrorKind::TooOld, ", older than its name keeps"));
        }
        if point.at > horizon || point.at == i64::MAX {
            return Err(refused(ErrorKind::TooNew, ", more than ten minutes ahead of the store's clock"));
        }
    }
    let points = ordered(series.points);
    Ok(Batch { identity: series.kept.identity(), kept: series.kept, kind: series.kind, name: name.id, keep, points })
}

/// Points in the order of their times, of a time given twice the one given
/// last; a run already in order is kept as it is.
fn ordered(mut points: Vec<Point>) -> Vec<Point> {
    if points.windows(2).all(|pair| pair[0].at < pair[1].at) {
        return points;
    }
    // a stable sort keeps the order they were given in among equal times
    points.sort_by_key(|point| point.at);
    let mut kept: Vec<Point> = Vec::with_capacity(points.len());
    for point in points {
        match kept.last_mut() {
            Some(last) if last.at == point.at => *last = point,
            _ => kept.push(point),
        }
    }
    kept
}

/// What a series' write needs of its row: what it packed, its version, the
/// newest time it has seen, whether it was ready, when it is due, why it
/// stopped, and its head.
pub(super) struct State {
    sealed_before: Option<i64>,
    version: i64,
    pub(super) max_seen: i64,
    ready: bool,
    due: Option<i64>,
    failure: Option<String>,
    extent: Extent,
    head: Option<Vec<u8>>,
}

const STATE: &str = "select s.sealed_before, s.version, s.max_seen, s.ready, s.due, s.failed_at, s.failure,
    s.head_count, s.head_start, s.head_end, h.body
    from series_state s left join heads h on h.series_id = s.series_id where s.series_id = ?1";
macro_rules! save_state {
    ($set:literal) => {
        concat!(
            "update series_state set max_seen = ?2, version = version + 1, head_count = ?3, head_start = ?4,",
            " head_end = ?5",
            $set,
            " where series_id = ?1"
        )
    };
}
const SAVE_STATE: &str = save_state!("");
const SAVE_STATE_READY: &str = save_state!(", ready = ?6");
const SAVE_STATE_DUE: &str = save_state!(", due = ?6");
const SAVE_STATE_BOTH: &str = save_state!(", ready = ?6, due = ?7");
const SAVING: &str = "write a series' state";
const SAVE_HEAD: &str =
    "insert into heads (series_id, body) values (?1, ?2) on conflict (series_id) do update set body = excluded.body";

impl State {
    pub(super) fn read(tx: &Tx<'_>, id: i64) -> Result<State> {
        let state = tx.prepare_cached(STATE).and_then(|mut select| {
            select
                .query_row(params![id], |row| {
                    let failed: Option<i64> = row.get(5)?;
                    let failure: Option<String> = row.get(6)?;
                    let count: i64 = row.get(7)?;
                    let (start, end): (Option<i64>, Option<i64>) = (row.get(8)?, row.get(9)?);
                    Ok(State {
                        sealed_before: row.get(0)?,
                        version: row.get(1)?,
                        max_seen: row.get(2)?,
                        ready: row.get(3)?,
                        due: row.get(4)?,
                        failure: failed.map(|_| failure.unwrap_or_default()),
                        extent: Extent {
                            count: usize::try_from(count).unwrap_or(usize::MAX),
                            start: start.unwrap_or(0),
                            end: end.unwrap_or(0),
                        },
                        head: row.get(10)?,
                    })
                })
                .optional()
        });
        state
            .map_err(|error| sql_error("read a series' state", error))?
            .ok_or_else(|| Error::corrupt("a series without its state"))
    }

    /// Refuses a write the series can no longer take.
    fn accepts(&self, points: &[Point]) -> Result<()> {
        if let Some(failure) = &self.failure {
            return Err(Error::new(ErrorKind::Suspended, format!("it stopped taking samples: {failure}")));
        }
        if let Some(sealed) = self.sealed_before
            && let Some(behind) = points.iter().find(|point| point.at < sealed)
        {
            let at = rfc3339(behind.at);
            return Err(Error::new(ErrorKind::TooOld, format!("sample at {at}, behind what its series has packed")));
        }
        if self.version == i64::MAX {
            return Err(Error::limit("its version cannot move once more"));
        }
        Ok(())
    }
}

/// Writes one series' samples into its head, and says whether it now holds
/// a block's worth.
fn write_series(tx: &Tx<'_>, zstd: &mut Zstd, batch: &Batch) -> Result<bool> {
    let (id, _) = registry::resolve(tx, batch)?;
    let state = State::read(tx, id)?;
    store(tx, zstd, id, &state, batch)
}

/// Writes the samples of `batch` into the head of series `id`, whose row
/// says `state`, and says whether it now holds a block's worth.
pub(super) fn store(tx: &Tx<'_>, zstd: &mut Zstd, id: i64, state: &State, batch: &Batch) -> Result<bool> {
    state.accepts(&batch.points)?;
    let (bytes, extent) = next_head(zstd, id, state, &batch.points)?;
    let max_seen = state.max_seen.max(batch.points[batch.points.len() - 1].at);
    let ready = ready(extent, max_seen);
    let first_due = expires(batch.points[0].at, batch.keep);
    let due = state.due.map_or(first_due, |due| due.min(first_due));
    let (count, start, end) = (extent.count as i64, extent.start, extent.end);
    // ready and due are indexed: naming either rewrites its index entry, even for the same value
    let saved = match (ready != state.ready, Some(due) != state.due) {
        (false, false) => execute(tx, SAVE_STATE, params![id, max_seen, count, start, end], SAVING),
        (true, false) => execute(tx, SAVE_STATE_READY, params![id, max_seen, count, start, end, ready], SAVING),
        (false, true) => execute(tx, SAVE_STATE_DUE, params![id, max_seen, count, start, end, due], SAVING),
        (true, true) => execute(tx, SAVE_STATE_BOTH, params![id, max_seen, count, start, end, ready, due], SAVING),
    };
    saved?;
    execute(tx, SAVE_HEAD, params![id, bytes], "write a series' head")?;
    Ok(ready)
}

/// The head after a write: the leading chunks the write does not reach kept
/// as they are, the rest decoded, merged with the samples and encoded again.
fn next_head(zstd: &mut Zstd, id: i64, state: &State, points: &[Point]) -> Result<(Vec<u8>, Extent)> {
    let chunks = match &state.head {
        Some(bytes) if state.extent.count > 0 => head::parse(id, state.extent, bytes)?,
        None if state.extent.count == 0 => Vec::new(),
        _ => return Err(Error::corrupt("a head that is not its state's")),
    };
    let kept = head::kept_before(&chunks, points[0].at);
    let mut rest = Vec::new();
    head::decode(zstd, &chunks[kept..], i64::MIN, i64::MAX, &mut rest)?;
    let merged = head::merge(&rest, points);
    let bytes = head::encode(zstd, id, &chunks[..kept], &merged)?;
    let kept_count: usize = chunks[..kept].iter().map(|chunk| chunk.head.count).sum();
    let start = chunks.first().filter(|_| kept > 0).map_or(merged[0].at, |chunk| chunk.head.start);
    Ok((bytes, Extent { count: kept_count + merged.len(), start, end: merged[merged.len() - 1].at }))
}

/// Whether a head holds a block's worth: a whole unit of its ladder behind
/// the newest time its series has seen, which no sample can enter any more
/// once packed. It is a guess from the head's extent, which packing checks
/// against the samples.
pub(super) fn ready(extent: Extent, max_seen: i64) -> bool {
    extent.count > 1 && unit_end(extent.start, unit_of(extent.count, extent.start, extent.end)) <= max_seen
}

#[cfg(test)]
#[path = "ingest_tests.rs"]
mod tests;
