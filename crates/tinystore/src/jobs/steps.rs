//! Steps: a handler's answers kept across its job's attempts, so that the
//! attempt after a retry or a crash runs only the steps left.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::claim::Lease;
use super::engine::Jobs;
use super::run::Run;
use super::values::check_size;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, ErrorKind, Result};

/// A step's name is at most this long.
const MAX_NAME: usize = 256;

const FIND_STEP: &str = "select answer from _tinystore_jobs_steps where job = ?1 and name = ?2";
/// A step is kept only while the lease of the attempt keeping it holds the
/// job, so that one whose lease another claim took keeps nothing.
const KEEP_STEP: &str = "insert into _tinystore_jobs_steps (job, name, answer, at)
    select ?1, ?2, ?3, ?4 where exists (select 1 from _tinystore_jobs_leases where id = ?1 and attempt = ?5)
    on conflict (job, name) do update set answer = excluded.answer, at = excluded.at";

pub(crate) fn run<T, E>(run: &Run, name: &str, work: impl FnOnce() -> Result<T, E>) -> Result<T, E>
where
    T: Serialize + DeserializeOwned,
    E: From<Error>,
{
    let describe = || format!("{}: step {name:?}", run.describe());
    if run.lease.cancelled() {
        return Err(cancelled_step().within(describe()).into());
    }
    let kept = kept(&run.jobs, &run.lease, name).map_err(|error| error.within(describe()))?;
    if let Some(kept) = kept {
        return answer_of(&kept).map_err(|error| error.within(describe()).into());
    }
    let answer = work()?;
    encoded(&answer)
        .and_then(|encoded| keep_encoded(&run.jobs, &run.lease, name, encoded))
        .map_err(|error| error.within(describe()))?;
    Ok(answer)
}

/// What a step of a run a cancel took fails with.
pub(crate) fn cancelled_step() -> Error {
    Error::new(ErrorKind::Conflict, "the job was cancelled while it ran")
}

/// A kept answer read back as the step's type.
pub(crate) fn answer_of<T: DeserializeOwned>(kept: &[u8]) -> Result<T> {
    serde_json::from_slice(kept)
        .map_err(|error| Error::invalid("it kept an answer that no longer reads").with_source(error))
}

/// A step's answer as it is kept: JSON.
pub(crate) fn encoded<T: Serialize>(answer: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(answer).map_err(|error| Error::invalid("an answer JSON cannot write").with_source(error))
}

/// The answer a job's run kept under a step's name, as JSON; none for a step
/// not yet kept.
pub(crate) fn kept(jobs: &Jobs, lease: &Lease, name: &str) -> Result<Option<Vec<u8>>> {
    check_name(name)?;
    let job = lease.id;
    jobs.file().read(|c| {
        c.prepare_cached(FIND_STEP)
            .and_then(|mut select| select.query_row(params![job, name], |row| row.get(0)).optional())
            .map_err(|error| sql_error("a step's kept answer", error))
    })
}

/// Keeps a step's answer, JSON, while the lease of the attempt keeping it
/// holds the job; one whose lease another claim took is a `conflict`.
pub(crate) fn keep_encoded(jobs: &Jobs, lease: &Lease, name: &str, encoded: Vec<u8>) -> Result<()> {
    let (bytes, write) = keeping(jobs, lease, name, encoded)?;
    jobs.file().write(bytes, write).and_then(kept_one)
}

/// [`keep_encoded`] without waiting: the write is queued, and `done` is
/// called with its answer on the thread that commits it.
#[cfg(feature = "async")]
pub(crate) fn keep_then(
    jobs: &Jobs,
    lease: &Lease,
    name: &str,
    encoded: Vec<u8>,
    done: impl FnOnce(Result<()>) + Send + 'static,
) {
    match keeping(jobs, lease, name, encoded) {
        Ok((bytes, write)) => jobs.file().submit(bytes, write, move |kept| done(kept.and_then(kept_one))),
        Err(error) => done(Err(error)),
    }
}

/// The write that keeps a step's answer, checked, and the bytes it holds.
fn keeping(
    jobs: &Jobs,
    lease: &Lease,
    name: &str,
    encoded: Vec<u8>,
) -> Result<(usize, impl FnOnce(&Tx<'_>) -> Result<usize> + Send + 'static)> {
    check_name(name)?;
    check_size(encoded.len(), "an answer")?;
    let (job, attempt, now, name, bytes) = (lease.id, lease.attempt, jobs.now(), name.to_owned(), encoded.len());
    let write = move |tx: &Tx<'_>| {
        tx.prepare_cached(KEEP_STEP)
            .and_then(|mut insert| insert.execute(params![job, name, encoded, now, attempt]))
            .map_err(|error| sql_error("a step's answer", error))
    };
    Ok((bytes, write))
}

/// Whether the keep's write kept the answer: none is a lease another claim
/// took.
fn kept_one(kept: usize) -> Result<()> {
    if kept == 0 {
        return Err(Error::new(ErrorKind::Conflict, "the job's lease ended and another claim or a change took it"));
    }
    Ok(())
}

fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(Error::invalid(format!("a step's name is 1 to {MAX_NAME} bytes")));
    }
    Ok(())
}
