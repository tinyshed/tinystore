//! `ingest`: series and their samples, written whole or not at all.
//!
//! ```text
//! check every series and gather its samples ─► what the names change ─► check each sample's time
//!     ─► one grouped write: the names made and keeps moved, each series found or registered,
//!        its head rewritten
//! ```
//!
//! A call is one savepoint of the file's grouped commit: a name it makes, a
//! keep it moves, a series it registers, its pairs and postings, and every
//! head it rewrites commit together, or nothing does. A refused series names itself by its labels, so
//! that the call can be sent again without it.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use rusqlite::{OptionalExtension, params};

use super::buckets::{bound_of, le};
use super::codec::Point;
use super::engine::{AHEAD, Engine, execute};
use super::head::{self, Extent};
use super::labels::Kept;
use super::ladder::{unit_end, unit_of};
use super::names::{Changes, KEEP, Name, checked_keep, millis, write_changes};
use super::registry;
use super::repeat::Packed;
use super::retention::expires;
use super::series::{Kind, Labels, Series};
use super::timestamp::rfc3339;
use crate::compress::frame::Zstd;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, ErrorKind, Reservation, Result, unix_millis};

/// What one call carries at most: more is sent in several.
const MOST_SAMPLES: usize = 10_000;
const MOST_BYTES: usize = 4 << 20;
/// What a sample weighs against the call's bytes, a time and a value.
pub(super) const SAMPLE_BYTES: usize = 16;
use crate::memory::PATIENCE;

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

/// A call's series checked and gathered by their labels, its time read,
/// before it reserves the store's memory their write holds: an awaited
/// ingest waits for those bytes on no thread.
pub(crate) struct Checked {
    now: i64,
    gathered: BTreeMap<[u8; 32], Gathered>,
    samples: usize,
    bytes: usize,
}

impl Checked {
    /// What the write holds of the store's memory.
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes as u64
    }
}

impl Engine {
    /// Writes every series of the call or none of them.
    pub(super) fn ingest(&self, series: &[Series]) -> Result<()> {
        let checked = self.check(series)?;
        let held = self.memory.reserve(checked.bytes(), "metrics: an ingest", PATIENCE)?;
        self.ingest_checked(checked, held)
    }

    /// The call's series checked within one ingest's bounds and gathered by
    /// their labels, its `now` read once.
    pub(super) fn check(&self, series: &[Series]) -> Result<Checked> {
        let now = self.now();
        let (gathered, samples, bytes) = gather(series)?;
        Ok(Checked { now, gathered, samples, bytes })
    }

    /// Writes series checked, every one or none, `_held` the memory their
    /// write holds until it ends.
    pub(super) fn ingest_checked(&self, checked: Checked, _held: Reservation) -> Result<()> {
        let Checked { now, gathered, samples, bytes } = checked;
        let wanted = wanted_names(&gathered)?;
        // A call refused for its times makes no name and moves no keep.
        let keeps = self.names.foreseen(&wanted);
        for series in gathered.values() {
            within_window(series, keeps.get(series.kept.name()).copied().unwrap_or(KEEP), now)?;
        }
        let settling = self.names.settling(&wanted)?;
        let names = settling.names();
        let batches = gathered.into_values().map(|series| batch(series, &names, now)).collect::<Result<Vec<_>>>()?;
        if batches.is_empty() {
            return Ok(());
        }
        let (ready, made) = self.write(batches, settling.changes(), bytes)?;
        settling.settled(made);
        if ready {
            self.wake();
        }
        self.probe.count("tinystore_metrics_samples_total", &[], samples as f64);
        Ok(())
    }

    /// The call's one write, its names' changes first, and whether a series
    /// of it now holds a block's worth, with the ids of the names it made.
    fn write(&self, mut batches: Vec<Batch>, names: Changes, bytes: usize) -> Result<(bool, HashMap<String, i64>)> {
        let zstd = self.codecs.take();
        let max_series = self.max_series;
        let (zstd, ready, made) = self.file.write(bytes, move |tx| {
            let mut zstd = zstd;
            let made = write_changes(tx, &names)?;
            let mut ready = false;
            for batch in &mut batches {
                if let Some(id) = made.get(batch.kept.name()) {
                    batch.name = *id;
                }
                let written = write_series(tx, &mut zstd, batch, max_series);
                ready |= written.map_err(|error| error.within(format!("metrics {}", batch.kept)))?;
            }
            Ok((zstd, ready, made))
        })?;
        self.codecs.give_back(zstd);
        Ok((ready, made))
    }
}

/// Checks each series and gathers its samples by its labels, within the
/// call's bounds: one series given in several parts is one. Gives the
/// samples given and the bytes they weigh beside them.
fn gather(series: &[Series]) -> Result<(BTreeMap<[u8; 32], Gathered>, usize, usize)> {
    let (mut gathered, mut samples, mut bytes) = (BTreeMap::new(), 0, 0);
    for given in series {
        let named = || format!("metrics {given}");
        super::instruments::refuse_the_stores_own(&given.name).map_err(|error| error.within(named()))?;
        let labels = bounded(given).map_err(|error| error.within(named()))?;
        let kept = Kept::of(&given.name, &labels).map_err(|error| error.within(named()))?;
        if given.samples.is_empty() {
            return Err(Error::invalid(format!("{}: a series without samples", named())));
        }
        let keep = given.keep.map(checked_keep).transpose().map_err(|error| error.within(named()))?;
        samples += given.samples.len();
        bytes += kept.bytes() + SAMPLE_BYTES * given.samples.len();
        within_one_ingest("an ingest", samples, bytes).map_err(|error| error.within("metrics"))?;
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
    Ok((gathered, samples, bytes))
}

/// Refuses `what` past the samples or the bytes one ingest carries, a sample
/// weighing its labels, its name among them, and `SAMPLE_BYTES`.
pub(super) fn within_one_ingest(what: &str, samples: usize, bytes: usize) -> Result<()> {
    if samples <= MOST_SAMPLES && bytes <= MOST_BYTES {
        return Ok(());
    }
    let error =
        Error::limit(format!("{what} past {MOST_SAMPLES} samples or {} MiB: send it in parts", MOST_BYTES >> 20));
    Err(error
        .naming("limit", "one ingest")
        .naming("wanted", samples.to_string())
        .naming("bound", MOST_SAMPLES.to_string()))
}

/// A series' labels, a histogram's bound among them written as JavaScript
/// writes its number, so that `5.0` from one exporter and `5` from another
/// are one bound; a histogram's series without one is refused.
fn bounded(series: &Series) -> Result<Cow<'_, Labels>> {
    if series.kind != Kind::Histogram {
        return Ok(Cow::Borrowed(&series.labels));
    }
    let bound = series.labels.get("le").and_then(|text| bound_of(text)).ok_or_else(|| {
        Error::invalid("a histogram's series counts the values at most its bound, `le`: a number of 0 or more, or +Inf")
    })?;
    let mut labels = series.labels.clone();
    labels.insert("le".to_owned(), le(bound));
    Ok(Cow::Owned(labels))
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
    within_window(&series, name.keeps(), now)?;
    let keep = millis(name.keeps());
    let points = ordered(series.points);
    Ok(Batch { identity: series.kept.identity(), kept: series.kept, kind: series.kind, name: name.id, keep, points })
}

/// Refuses a sample older than `keep` or more than ten minutes ahead.
fn within_window(series: &Gathered, keep: Duration, now: i64) -> Result<()> {
    let (cutoff, horizon) = (now.saturating_sub(millis(keep)), now.saturating_add(millis(AHEAD)));
    for point in &series.points {
        let refused =
            |kind, why| Error::new(kind, format!("metrics {}: sample at {}{why}", series.kept, rfc3339(point.at)));
        if point.at < cutoff {
            return Err(refused(ErrorKind::TooOld, ", older than its name keeps"));
        }
        if point.at > horizon || point.at == i64::MAX {
            return Err(refused(ErrorKind::TooNew, ", more than ten minutes ahead of the store's clock"));
        }
    }
    Ok(())
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

/// What a series' write needs of its row: what it packed, its version, its
/// oldest sample and the newest time it has seen, whether it was ready, when
/// it is due, why it stopped, and its head.
pub(super) struct State {
    sealed_before: Option<i64>,
    version: i64,
    oldest: i64,
    pub(super) max_seen: i64,
    ready: bool,
    due: Option<i64>,
    failure: Option<String>,
    extent: Extent,
    head: Option<Vec<u8>>,
}

const STATE: &str = "select s.sealed_before, s.version, s.max_seen, s.ready, s.due, s.failed_at, s.failure,
    s.head_count, s.head_start, s.head_end, h.body, s.oldest
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
/// A series' oldest sample, indexed by its name, written only when a sample
/// before it moves it.
const SET_OLDEST: &str = "update series_state set oldest = ?2 where series_id = ?1";
/// A name's newest sample, which a write past it moves.
const SET_NEWEST: &str = "update names set newest = ?2 where id = ?1 and (newest is null or newest < ?2)";
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
                        oldest: row.get(11)?,
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

    /// Takes out of `points` those sent again behind what the series has
    /// packed, each the sample packed at its time, and refuses the call when
    /// another reaches behind it. A series that stopped is `accepts`' to refuse.
    fn drop_repeats(&self, tx: &Tx<'_>, zstd: &mut Zstd, id: i64, points: &mut Vec<Point>) -> Result<()> {
        match self.sealed_before {
            Some(sealed) if self.failure.is_none() => Packed::of(tx, zstd, id, sealed).drop_repeats(points),
            _ => Ok(()),
        }
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
/// a block's worth. Its samples sent again behind what it has packed leave
/// the batch first, so that a call retried after packing changes nothing.
fn write_series(tx: &Tx<'_>, zstd: &mut Zstd, batch: &mut Batch, max_series: i64) -> Result<bool> {
    let (id, _) = registry::resolve(tx, batch, max_series)?;
    let state = State::read(tx, id)?;
    state.drop_repeats(tx, zstd, id, &mut batch.points)?;
    if batch.points.is_empty() {
        return Ok(false);
    }
    store(tx, zstd, id, &state, batch)
}

/// Writes the samples of `batch` into the head of series `id`, whose row
/// says `state`, and says whether it now holds a block's worth.
pub(super) fn store(tx: &Tx<'_>, zstd: &mut Zstd, id: i64, state: &State, batch: &Batch) -> Result<bool> {
    state.accepts(&batch.points)?;
    let Some((bytes, extent)) = next_head(zstd, id, state, &batch.points)? else { return Ok(state.ready) };
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
    if batch.points[0].at < state.oldest {
        execute(tx, SET_OLDEST, params![id, batch.points[0].at], "move a series' oldest sample")?;
    }
    execute(tx, SET_NEWEST, params![batch.name, max_seen], "move a name's newest sample")?;
    Ok(ready)
}

/// The head after a write: the leading chunks the write does not reach kept
/// as they are, the rest decoded, merged with the samples and encoded again;
/// none when each sample is the one the head holds at its time, bit for bit,
/// as a batch sent again is, so that its write moves not even the version.
fn next_head(zstd: &mut Zstd, id: i64, state: &State, points: &[Point]) -> Result<Option<(Vec<u8>, Extent)>> {
    let chunks = match &state.head {
        Some(bytes) if state.extent.count > 0 => head::parse(id, state.extent, bytes)?,
        None if state.extent.count == 0 => Vec::new(),
        _ => return Err(Error::corrupt("a head that is not its state's")),
    };
    let kept = head::kept_before(&chunks, points[0].at);
    let mut rest = Vec::new();
    head::decode(zstd, &chunks[kept..], i64::MIN, i64::MAX, &mut rest)?;
    let merged = head::merge(&rest, points);
    let unchanged = |one: &Point, other: &Point| one.at == other.at && one.value.to_bits() == other.value.to_bits();
    if merged.len() == rest.len() && merged.iter().zip(&rest).all(|(one, other)| unchanged(one, other)) {
        return Ok(None);
    }
    let bytes = head::encode(zstd, id, &chunks[..kept], &merged)?;
    let kept_count: usize = chunks[..kept].iter().map(|chunk| chunk.head.count).sum();
    let start = chunks.first().filter(|_| kept > 0).map_or(merged[0].at, |chunk| chunk.head.start);
    Ok(Some((bytes, Extent { count: kept_count + merged.len(), start, end: merged[merged.len() - 1].at })))
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
