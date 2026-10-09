//! Steps: a handler's answers kept across its job's attempts, so that the
//! attempt after a retry or a crash runs only the steps left.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::run::Run;
use super::values::check_size;
use crate::sqlite::sql_error;
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
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(Error::invalid(format!("a step's name is 1 to {MAX_NAME} bytes")).within(describe()).into());
    }
    if run.lease.cancelled() {
        return Err(Error::new(ErrorKind::Conflict, "the job was cancelled while it ran").within(describe()).into());
    }
    let job = run.lease.id;
    let kept: Option<Vec<u8>> = run
        .jobs
        .file()
        .read(|c| {
            c.prepare_cached(FIND_STEP)
                .and_then(|mut select| select.query_row(params![job, name], |row| row.get(0)).optional())
                .map_err(|error| sql_error("a step's kept answer", error))
        })
        .map_err(|error| error.within(describe()))?;
    if let Some(kept) = kept {
        return serde_json::from_slice(&kept).map_err(|error| {
            Error::invalid("it kept an answer that no longer reads").with_source(error).within(describe()).into()
        });
    }
    let answer = work()?;
    keep(run, name, &answer).map_err(|error| error.within(describe()))?;
    Ok(answer)
}

fn keep<T: Serialize>(run: &Run, name: &str, answer: &T) -> Result<()> {
    let encoded =
        serde_json::to_vec(answer).map_err(|error| Error::invalid("an answer JSON cannot write").with_source(error))?;
    check_size(encoded.len(), "an answer")?;
    let (job, attempt, now, name) = (run.lease.id, run.lease.attempt, run.jobs.now(), name.to_owned());
    let kept = run.jobs.file().write(encoded.len(), move |tx| {
        tx.prepare_cached(KEEP_STEP)
            .and_then(|mut insert| insert.execute(params![job, name, encoded, now, attempt]))
            .map_err(|error| sql_error("a step's answer", error))
    })?;
    if kept == 0 {
        return Err(Error::new(ErrorKind::Conflict, "the job's lease ended and another claim took it"));
    }
    Ok(())
}
