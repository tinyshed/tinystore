//! The rows of jobs.db that adding, changing, finding and keeping jobs read
//! and write: one statement a step, each a named constant. Claims and their
//! settlements are claim.rs's.

use rusqlite::{Connection, OptionalExtension, Params, params};

use super::values::Kept;
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// A due job whose group has no room is parked: its next moves `PARKED_BY`
/// later, past every time a job may have, so that the next claim does not meet
/// it again, and moves back when its group frees a place.
///
/// The parked index holds the rows from `PARKED_FROM` on, and a query reaches
/// it only by naming that bound as the index does, in its digits: the primary
/// key's range looks as good to a planner without statistics, and walks the
/// parked rows of every group.
pub(crate) const PARKED_BY: i64 = 1 << 60;
pub(crate) const PARKED_FROM: i64 = 1 << 59;
macro_rules! parked_from {
    () => {
        "576460752303423488"
    };
}

/// When a job is due: its next, or a parked job's before it was parked.
pub(crate) fn due_of(next: i64) -> i64 {
    if next >= PARKED_FROM { next - PARKED_BY } else { next }
}

const REGISTER_QUEUE: &str =
    "insert into _tinystore_jobs_queues (name, kind) values (?1, ?2) on conflict (name) do nothing";
const QUEUE_NAMED: &str = "select id, kind, in_group from _tinystore_jobs_queues where name = ?1";
const COUNT_WAITING: &str = "select waiting from _tinystore_jobs_queues where id = ?1";
const KEEP_IN_GROUP: &str = "update _tinystore_jobs_queues set in_group = ?2 where id = ?1";
const UNPARK_JOBS: &str =
    concat!("update _tinystore_jobs set next = next - ?2 where queue = ?1 and next >= ", parked_from!());
const UNPARK_KEYS: &str =
    concat!("update _tinystore_jobs_keys set next = next - ?2 where queue = ?1 and next >= ", parked_from!());
const RESERVE_IDS: &str = "update _tinystore_jobs_meta set value = value + ?1 where name = 'ids' returning value";
/// A lease the file still holds at open belongs to a process that died: its
/// attempt counts, and its job is due again at once.
const COUNT_DEAD_ATTEMPTS: &str = "update _tinystore_jobs set attempt = l.attempt from _tinystore_jobs_leases l
    where _tinystore_jobs.queue = l.queue and _tinystore_jobs.next = l.next and _tinystore_jobs.id = l.id";
const DROP_DEAD_LEASES: &str = "delete from _tinystore_jobs_leases";

const JOB_BY_KEY: &str = "select j.next, j.id, j.again, j.repeat, j.spill
    from _tinystore_jobs_keys k join _tinystore_jobs j on j.queue = k.queue and j.next = k.next and j.id = k.id
    where k.queue = ?1 and k.key = ?2";
const LEASE_HELD: &str = "select 1 from _tinystore_jobs_leases where id = ?1 and until > ?2";
const FAILED_KEY: &str = "select 1 from _tinystore_jobs_failed where queue = ?1 and key = ?2";
const DONE_KEY: &str = "select 1 from _tinystore_jobs_done where queue = ?1 and key = ?2 and until > ?3";

const INSERT_JOB: &str = "insert into _tinystore_jobs (queue, next, id, key, at, attempt, repeat, value, spill, grp)
    values (?1, ?2, ?3, ?4, ?2, 0, ?5, ?6, ?7, ?8)";
const INSERT_SPILLED: &str = "insert into _tinystore_jobs_spilled (id, value) values (?1, ?2)";
const DELETE_SPILLED: &str = "delete from _tinystore_jobs_spilled where id = ?1";
/// A key's row: where its job lies, replacing a key a job left behind.
const INSERT_KEY: &str = "insert into _tinystore_jobs_keys (queue, key, next, id) values (?1, ?2, ?3, ?4)
    on conflict (queue, key) do update set next = excluded.next, id = excluded.id";
const MOVE_KEY: &str = "update _tinystore_jobs_keys set next = ?3 where queue = ?1 and key = ?2";
const DROP_KEY: &str = "delete from _tinystore_jobs_keys where queue = ?1 and key = ?2";
/// A set's job made new: its value and time, its attempts from one and
/// nothing of what its last run left; a repeat or a group the set does not name
/// stays.
const RENEW_JOB: &str = "update _tinystore_jobs
    set next = ?4, at = ?4, attempt = 0, again = null, error = null, value = ?5, spill = ?6,
        repeat = coalesce(?7, repeat), grp = coalesce(?8, grp)
    where queue = ?1 and next = ?2 and id = ?3";
/// An update's job: its value, and what else the update named.
const UPDATE_JOB: &str = "update _tinystore_jobs
    set next = ?4, at = ?4, value = ?5, spill = ?6, repeat = coalesce(?7, repeat), grp = coalesce(?8, grp)
    where queue = ?1 and next = ?2 and id = ?3";
/// A set while the job runs: the value and repeat its next run takes, and when
/// that run is, which its settlement reads.
const ASK_AGAIN: &str = "update _tinystore_jobs
    set again = ?4, value = ?5, spill = ?6, repeat = coalesce(?7, repeat), grp = coalesce(?8, grp)
    where queue = ?1 and next = ?2 and id = ?3";
const DROP_FAILED: &str = "delete from _tinystore_jobs_failed where queue = ?1 and key = ?2 returning spill, grp";
const CANCEL_JOB: &str = "delete from _tinystore_jobs where (queue, next, id) in (
        select k.queue, k.next, k.id from _tinystore_jobs_keys k where k.queue = ?1 and k.key = ?2)
    returning id, spill, next, grp";
const DROP_LEASE: &str = "delete from _tinystore_jobs_leases where id = ?1";
const DROP_STEPS: &str = "delete from _tinystore_jobs_steps where job = ?1";
const FIRST_PARKED: &str = concat!(
    "select next, id, key from _tinystore_jobs indexed by _tinystore_jobs_parked
    where queue = ?1 and grp = ?2 and next >= ",
    parked_from!(),
    " order by next, id limit 1"
);
const MOVE_ROW: &str = "update _tinystore_jobs set next = ?4 where queue = ?1 and next = ?2 and id = ?3";
const MOVE_LEASE: &str = "update _tinystore_jobs_leases set next = ?2 where id = ?1";

/// What a write needs of a job's row.
#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub(crate) next: i64,
    pub(crate) id: i64,
    pub(crate) again: Option<i64>,
    pub(crate) repeat: Option<String>,
    pub(crate) spill: Option<i64>,
}

/// A job a write adds.
#[derive(Debug)]
pub(crate) struct NewJob<'a> {
    pub(crate) queue: i64,
    pub(crate) id: i64,
    pub(crate) key: Option<&'a str>,
    pub(crate) at: i64,
    pub(crate) repeat: Option<&'a str>,
    pub(crate) group: Option<&'a str>,
    pub(crate) value: &'a Kept,
}

/// What a write gives a job that is there: its value, its time and repeat,
/// and its group.
#[derive(Debug)]
pub(crate) struct Change<'a> {
    pub(crate) next: i64,
    pub(crate) repeat: Option<&'a str>,
    pub(crate) group: Option<&'a str>,
    pub(crate) value: &'a Kept,
}

/// Registers a queue on its first open, and answers its id, its kind and the
/// group bound it was last opened with.
pub(crate) fn queue(c: &Connection, name: &str, kind: &str) -> Result<(i64, String, i64)> {
    execute(c, REGISTER_QUEUE, params![name, kind], "a queue's registration")?;
    c.prepare_cached(QUEUE_NAMED)
        .and_then(|mut select| select.query_row([name], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))))
        .map_err(|error| sql_error("a queue's registration", error))
}

/// Keeps the group bound a queue opens with, and gives back the jobs another
/// one parked, since only a settlement in their group would give them back
/// otherwise.
pub(crate) fn keep_group_bound(c: &Connection, queue: i64, was: i64, is: i64) -> Result<()> {
    if was == is {
        return Ok(());
    }
    execute(c, UNPARK_JOBS, params![queue, PARKED_BY], "the jobs another group bound parked")?;
    execute(c, UNPARK_KEYS, params![queue, PARKED_BY], "the jobs another group bound parked")?;
    execute(c, KEEP_IN_GROUP, params![queue, is], "a queue's group bound")
}

/// Reserves `block` job ids and answers the end of the block.
pub(crate) fn reserve_ids(c: &Connection, block: i64) -> Result<i64> {
    c.prepare_cached(RESERVE_IDS)
        .and_then(|mut update| update.query_row([block], |row| row.get(0)))
        .map_err(|error| sql_error("a block of job ids", error))
}

pub(crate) fn end_dead_leases(c: &Connection) -> Result<()> {
    execute(c, COUNT_DEAD_ATTEMPTS, [], "the leases of a process that died")?;
    execute(c, DROP_DEAD_LEASES, [], "the leases of a process that died")
}

pub(crate) fn job_by_key(c: &Connection, queue: i64, key: &str) -> Result<Option<Row>> {
    c.prepare_cached(JOB_BY_KEY)
        .and_then(|mut select| {
            select
                .query_row(params![queue, key], |row| {
                    Ok(Row {
                        next: row.get(0)?,
                        id: row.get(1)?,
                        again: row.get(2)?,
                        repeat: row.get(3)?,
                        spill: row.get(4)?,
                    })
                })
                .optional()
        })
        .map_err(|error| sql_error("the job under an id", error))
}

/// Whether a claim holds the job at `now`; a lease that has ended holds
/// nothing, and the next claim takes the job.
pub(crate) fn lease_held(c: &Connection, id: i64, now: i64) -> Result<bool> {
    exists(c, LEASE_HELD, params![id, now])
}

pub(crate) fn failed_key(c: &Connection, queue: i64, key: &str) -> Result<bool> {
    exists(c, FAILED_KEY, params![queue, key])
}

pub(crate) fn done_key(c: &Connection, queue: i64, key: &str, now: i64) -> Result<bool> {
    exists(c, DONE_KEY, params![queue, key, now])
}

/// Refuses a job past the queue's `maxWaiting`. The count is the queue's row,
/// which triggers keep, so that a transaction sees its own adds.
pub(crate) fn check_room(c: &Connection, queue: i64, max_waiting: u64) -> Result<()> {
    let waiting: i64 = c
        .prepare_cached(COUNT_WAITING)
        .and_then(|mut select| select.query_row([queue], |row| row.get(0)))
        .map_err(|error| sql_error("the jobs a queue holds", error))?;
    if u64::try_from(waiting).unwrap_or(0) >= max_waiting {
        return Err(Error::limit(format!("the queue holds {waiting} jobs, its maxWaiting")));
    }
    Ok(())
}

/// Adds a job's row, its value spilled to a row of its own past the inline
/// bound, and its key where it has one.
pub(crate) fn insert(c: &Connection, job: &NewJob<'_>) -> Result<()> {
    let spill = spill(c, job.id, job.value)?;
    let params = params![job.queue, job.at, job.id, job.key, job.repeat, job.value.inline(), spill, job.group];
    execute(c, INSERT_JOB, params, "a job's row")?;
    if let Some(key) = job.key {
        execute(c, INSERT_KEY, params![job.queue, key, job.at, job.id], "a job's id")?;
    }
    Ok(())
}

/// Makes the job a set names new: its value, time, repeat and group, its
/// attempts from one, its steps gone with the run they belonged to.
pub(crate) fn renew(c: &Connection, queue: i64, key: &str, there: &Row, change: &Change<'_>) -> Result<()> {
    drop_spill(c, there.spill)?;
    let spill = spill(c, there.id, change.value)?;
    let params =
        params![queue, there.next, there.id, change.next, change.value.inline(), spill, change.repeat, change.group];
    execute(c, RENEW_JOB, params, "a set's job")?;
    execute(c, DROP_STEPS, [there.id], "a set's job: the steps of its last run")?;
    follow(c, queue, key, there.next, change.next)
}

/// Gives a job that has not started what an update names; a repeat or a group
/// left out stays as it was.
pub(crate) fn update(c: &Connection, queue: i64, key: &str, there: &Row, change: &Change<'_>) -> Result<()> {
    drop_spill(c, there.spill)?;
    let spill = spill(c, there.id, change.value)?;
    let params =
        params![queue, there.next, there.id, change.next, change.value.inline(), spill, change.repeat, change.group];
    execute(c, UPDATE_JOB, params, "an update's job")?;
    follow(c, queue, key, there.next, change.next)
}

/// Asks one run more of a job that runs: its next run takes the new value and
/// repeat, at `again`, or at the repeat's next time when that is none.
pub(crate) fn ask_again(
    c: &Connection,
    queue: i64,
    there: &Row,
    again: Option<i64>,
    change: &Change<'_>,
) -> Result<()> {
    drop_spill(c, there.spill)?;
    let spill = spill(c, there.id, change.value)?;
    let params = params![queue, there.next, there.id, again, change.value.inline(), spill, change.repeat, change.group];
    execute(c, ASK_AGAIN, params, "a set of a running job")
}

/// Deletes the failed job under a key and its spilled value, and answers its
/// group when there was one.
pub(crate) fn drop_failed(c: &Connection, queue: i64, key: &str) -> Result<Option<Option<String>>> {
    let dropped: Option<(Option<i64>, Option<String>)> = c
        .prepare_cached(DROP_FAILED)
        .and_then(|mut delete| delete.query_row(params![queue, key], |row| Ok((row.get(0)?, row.get(1)?))).optional())
        .map_err(|error| sql_error("a failed job", error))?;
    let Some((spilled, group)) = dropped else {
        return Ok(None);
    };
    drop_spill(c, spilled)?;
    Ok(Some(group))
}

/// A job a cancel took from the queue's rows.
#[derive(Debug)]
pub(crate) struct Taken {
    pub(crate) id: i64,
    pub(crate) group: Option<String>,
    /// Whether it held, or was due to take, its group's place.
    pub(crate) placed: bool,
}

/// Deletes the job under a key, waiting or running, with its lease, its
/// spilled value and its key.
pub(crate) fn cancel(c: &Connection, queue: i64, key: &str) -> Result<Option<Taken>> {
    let taken: Option<(i64, Option<i64>, i64, Option<String>)> = c
        .prepare_cached(CANCEL_JOB)
        .and_then(|mut delete| {
            delete
                .query_row(params![queue, key], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
                .optional()
        })
        .map_err(|error| sql_error("a cancel", error))?;
    let Some((id, spilled, next, group)) = taken else {
        return Ok(None);
    };
    execute(c, DROP_LEASE, [id], "a cancel: the job's lease")?;
    drop_spill(c, spilled)?;
    execute(c, DROP_KEY, params![queue, key], "a cancel: the job's id")?;
    Ok(Some(Taken { id, group, placed: next < PARKED_FROM }))
}

/// Gives the first parked job of a group back its time, once the group has
/// freed a place.
pub(crate) fn unpark_one(c: &Connection, queue: i64, group: &str) -> Result<()> {
    let first: Option<(i64, i64, Option<String>)> = c
        .prepare_cached(FIRST_PARKED)
        .and_then(|mut select| {
            select.query_row(params![queue, group], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()
        })
        .map_err(|error| sql_error("a group's first parked job", error))?;
    let Some((next, id, key)) = first else {
        return Ok(());
    };
    move_to(c, queue, (next, id, key.as_deref()), next - PARKED_BY)
}

/// Moves a job's row to `to`, with its key and a lease it outlived.
pub(crate) fn move_to(c: &Connection, queue: i64, (next, id, key): (i64, i64, Option<&str>), to: i64) -> Result<()> {
    execute(c, MOVE_ROW, params![queue, next, id, to], "a job's move")?;
    execute(c, MOVE_LEASE, params![id, to], "a job's move: its lease")?;
    match key {
        Some(key) => follow(c, queue, key, next, to),
        None => Ok(()),
    }
}

/// Moves a job's key along when the job's row moves.
pub(crate) fn follow(c: &Connection, queue: i64, key: &str, from: i64, to: i64) -> Result<()> {
    if from == to {
        return Ok(());
    }
    execute(c, MOVE_KEY, params![queue, key, to], "a job's id")
}

pub(crate) fn drop_spill(c: &Connection, spilled: Option<i64>) -> Result<()> {
    match spilled {
        Some(id) => execute(c, DELETE_SPILLED, [id], "a large value"),
        None => Ok(()),
    }
}

pub(crate) fn drop_steps(c: &Connection, job: i64) -> Result<()> {
    execute(c, DROP_STEPS, [job], "the steps of a run that ended")
}

/// Writes a value past the inline bound to a row of its own under the job's
/// id, and answers that id.
fn spill(c: &Connection, id: i64, value: &Kept) -> Result<Option<i64>> {
    match value {
        Kept::Inline(_) => Ok(None),
        Kept::Spilled(bytes) => {
            execute(c, INSERT_SPILLED, params![id, bytes], "a large value")?;
            Ok(Some(id))
        }
    }
}

pub(crate) fn execute(c: &Connection, sql: &str, params: impl Params, what: &str) -> Result<()> {
    c.prepare_cached(sql)
        .and_then(|mut statement| statement.execute(params))
        .map(drop)
        .map_err(|error| sql_error(what, error))
}

pub(crate) fn exists(c: &Connection, sql: &str, params: impl Params) -> Result<bool> {
    c.prepare_cached(sql).and_then(|mut select| select.exists(params)).map_err(|error| sql_error("a read", error))
}
