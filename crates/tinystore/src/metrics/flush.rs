//! The write of the instruments' totals: every 15 seconds, at `flush` and
//! when the store closes, every live total at one time, so that series
//! written together share their clocks.
//!
//! A slot's series — a counter's own, a timer's count, sum, longest and
//! buckets — are written in a savepoint of their own: they go together, and
//! what the store refuses of one slot is said once while the others are
//! written. A write that fails whole is tried again at the next, which
//! writes the totals that grew meanwhile.
//!
//! A run's zero lies after every sample its series holds, and its count
//! after the zero, so that a store opened again within the millisecond it
//! closed, or a clock moved back, never writes over a sample of the run
//! before.

use std::collections::BTreeMap;
use std::sync::Arc;

use rusqlite::params;

use super::codec::Point;
use super::engine::{Engine, execute, lock};
use super::ingest::{Batch, State, store};
use super::names::millis;
use super::registry;
use super::totals::{Outcome, Planned, Told, Written};
use crate::compress::frame::Zstd;
use crate::sqlite::Tx;
use crate::{Error, ErrorKind, Result};

/// The slots one transaction writes at most, so that the writer is never
/// held for long.
const SLOTS_A_WRITE: usize = 256;
/// What a sample of a write weighs against the store's memory: its series'
/// labels are its slot's, already held.
const SAMPLE_BYTES: usize = 64;

const DESCRIBE: &str = "update names set unit = coalesce(?2, unit), help = coalesce(?3, help),
    buckets = coalesce(?4, buckets) where name = ?1";
const SAVEPOINT: &str = "savepoint metrics_instrument";
const RELEASE: &str = "release metrics_instrument";
const ROLLBACK_TO: &str = "rollback to metrics_instrument";

/// A series' sample of a write, with what its batch needs of its name.
struct Sample {
    written: Arc<Written>,
    name: i64,
    keep: i64,
    zero: Option<i64>,
    value: f64,
}

/// A slot's samples of a write, or why it has none.
type Job = Result<Vec<Sample>>;

impl Engine {
    /// Writes every live total of the instruments, and what their names say
    /// of themselves.
    pub(super) fn write_totals(&self) -> Result<()> {
        let _one = lock(&self.writing);
        let now = self.now();
        let planned = self.totals.collect(now);
        let described = self.totals.take_descriptions();
        if planned.is_empty() && described.is_empty() {
            return Ok(());
        }
        let jobs: Vec<Job> = planned.iter().map(|plan| self.job(plan)).collect();
        let (outcomes, failure) = self.write_jobs(jobs, &described, now);
        if failure.is_some() {
            self.totals.give_back_descriptions(described);
        }
        self.totals.settle(planned, outcomes);
        failure.map_or(Ok(()), |error| Err(error.within("metrics: the instruments' write")))
    }

    /// A slot's samples, each with its name's id and keep.
    fn job(&self, plan: &Planned) -> Job {
        let samples = plan.values.iter().map(|value| {
            let written = Arc::clone(&value.written);
            let name = self.names.get(written.kept.name());
            let name =
                name.ok_or_else(|| Error::internal(format!("{}: a name its instrument did not open", written.kept)))?;
            let keep = millis(name.keeps());
            Ok(Sample { written, name: name.id, keep, zero: value.zero, value: value.value })
        });
        samples.collect()
    }

    /// Writes the jobs a few hundred slots a transaction, the descriptions
    /// with the first, and gives each slot's outcome and the failure that
    /// stopped the rest.
    fn write_jobs(
        &self,
        jobs: Vec<Job>,
        described: &BTreeMap<String, Told>,
        now: i64,
    ) -> (Vec<Outcome>, Option<Error>) {
        let mut outcomes = Vec::with_capacity(jobs.len());
        let mut failure = None;
        let mut ready = false;
        let mut jobs = jobs.into_iter().peekable();
        let mut first = true;
        while first || jobs.peek().is_some() {
            let chunk: Vec<Job> = jobs.by_ref().take(SLOTS_A_WRITE).collect();
            let count = chunk.len();
            let descriptions = if first { described.clone() } else { BTreeMap::new() };
            first = false;
            if failure.is_some() {
                outcomes.extend((0..count).map(|_| Outcome::Failed));
                continue;
            }
            match self.write_chunk(chunk, descriptions, now) {
                Ok((done, more_ready)) => {
                    outcomes.extend(done);
                    ready |= more_ready;
                }
                Err(error) => {
                    outcomes.extend((0..count).map(|_| Outcome::Failed));
                    failure = Some(error);
                }
            }
        }
        if ready {
            self.wake();
        }
        (outcomes, failure)
    }

    /// One transaction of the write: what names say of themselves, then
    /// each slot in a savepoint of its own.
    fn write_chunk(
        &self,
        chunk: Vec<Job>,
        described: BTreeMap<String, Told>,
        now: i64,
    ) -> Result<(Vec<Outcome>, bool)> {
        let samples: usize = chunk.iter().map(|job| job.as_ref().map_or(0, Vec::len)).sum();
        let _held =
            self.memory.reserve((samples * SAMPLE_BYTES) as u64, "metrics: the instruments' write", PATIENCE)?;
        let zstd = self.codecs.take();
        let (zstd, done) = self.file.write(samples * SAMPLE_BYTES, move |tx| {
            let mut zstd = zstd;
            for (name, description) in &described {
                let Told { unit, help, buckets } = description;
                execute(tx, DESCRIBE, params![name, unit, help, buckets], "describe a name")?;
            }
            let mut ready = false;
            let mut outcomes = Vec::with_capacity(chunk.len());
            for job in chunk {
                let outcome = match job {
                    Ok(samples) => write_slot(tx, &mut zstd, &samples, now, &mut ready)?,
                    Err(error) => Outcome::Refused(error),
                };
                outcomes.push(outcome);
            }
            Ok((zstd, (outcomes, ready)))
        })?;
        self.codecs.give_back(zstd);
        Ok(done)
    }
}

/// How long the instruments' write waits for the store's memory.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

/// Writes one slot's series in a savepoint: all of them, or none and the
/// error that refused one. An error of the file rather than of the series
/// fails the whole write.
fn write_slot(tx: &Tx<'_>, zstd: &mut Zstd, samples: &[Sample], now: i64, ready: &mut bool) -> Result<Outcome> {
    execute(tx, SAVEPOINT, params![], "start an instrument's write")?;
    let mut any_ready = false;
    let written = samples.iter().try_for_each(|sample| {
        any_ready |= write_sample(tx, zstd, sample, now).map_err(|error| error.within(&sample.written.kept))?;
        Ok::<_, Error>(())
    });
    match written {
        Ok(()) => {
            execute(tx, RELEASE, params![], "end an instrument's write")?;
            *ready |= any_ready;
            Ok(Outcome::Written)
        }
        Err(error) => {
            execute(tx, ROLLBACK_TO, params![], "undo an instrument's write")?;
            execute(tx, RELEASE, params![], "end an instrument's write")?;
            if refuses_the_series(error.kind()) { Ok(Outcome::Refused(error)) } else { Err(error) }
        }
    }
}

/// Whether an error is about a series, which the write leaves out, rather
/// than about the file, which fails it.
fn refuses_the_series(kind: ErrorKind) -> bool {
    use ErrorKind::{Conflict, Corrupt, Invalid, Limit, NotFound, Suspended, TooNew, TooOld};
    matches!(kind, Invalid | NotFound | Conflict | TooOld | TooNew | Limit | Corrupt | Suspended)
}

/// Writes one series' sample at `now`, and the zero its run begins with
/// before it, each placed after what the series already holds.
fn write_sample(tx: &Tx<'_>, zstd: &mut Zstd, sample: &Sample, now: i64) -> Result<bool> {
    let cutoff = now.saturating_sub(sample.keep);
    let mut points = Vec::with_capacity(2);
    if let Some(zero) = sample.zero {
        points.push(Point { at: zero.max(cutoff.saturating_add(1)).min(now.saturating_sub(1)), value: 0.0 });
    }
    points.push(Point { at: now, value: sample.value });
    let written = &sample.written;
    let mut batch = Batch {
        kept: written.kept.clone(),
        identity: written.identity,
        kind: written.kind,
        name: sample.name,
        keep: sample.keep,
        points,
    };
    let (id, fresh) = registry::resolve(tx, &batch)?;
    let state = State::read(tx, id)?;
    if !fresh {
        place(&mut batch.points, state.max_seen);
    }
    store(tx, zstd, id, &state, &batch)
}

/// Moves a write's samples after the newest its series holds: a zero after
/// it, and a value at it at the earliest, which a later total replaces.
///
/// ```text
/// newest held 100    zero 90, count 100  →  zero 101, count 102
///                    count 95            →  count 100
/// ```
fn place(points: &mut [Point], newest: i64) {
    match points {
        [zero, count] => {
            zero.at = zero.at.max(newest.saturating_add(1));
            count.at = count.at.max(zero.at.saturating_add(1));
        }
        [count] => count.at = count.at.max(newest),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_lands_after_what_its_series_holds() {
        let points = |times: &[i64]| -> Vec<Point> { times.iter().map(|at| Point { at: *at, value: 0.0 }).collect() };
        let placed = |mut points: Vec<Point>, newest| {
            place(&mut points, newest);
            points.iter().map(|point| point.at).collect::<Vec<_>>()
        };
        assert_eq!(placed(points(&[90, 100]), 100), [101, 102], "a zero never on the run before's last sample");
        assert_eq!(placed(points(&[90, 100]), 50), [90, 100], "nothing moves behind what the series holds");
        assert_eq!(placed(points(&[95]), 100), [100], "a total replaces the one at the same time");
        assert_eq!(placed(points(&[95]), 10), [95]);
    }
}
