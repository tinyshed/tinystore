//! The rows of jobs.db that adding, changing, finding and keeping jobs read
//! and write: one statement a step, each a named constant. Claims and their
//! settlements are claim.rs's.

use rusqlite::{Connection, OptionalExtension, Params, params};

use super::values::Kept;
use crate::Result;
use crate::limits::Limit;
use crate::sqlite::sql_error;

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
const NEXT_ID: &str = "update _tinystore_jobs_meta set value = value + 1 where name = 'ids'";
/// The id `NEXT_ID` took, read back by a select rather than `returning`,
/// which costs every add a microsecond more.
const IDS: &str = "select value from _tinystore_jobs_meta where name = 'ids'";
/// A lease the file still holds at open belongs to a process that died: its
/// attempt counts, and its job is due again at once.
const COUNT_DEAD_ATTEMPTS: &str = "update _tinystore_jobs set attempt = l.attempt from _tinystore_jobs_leases l
    where _tinystore_jobs.queue = l.queue and _tinystore_jobs.next = l.next and _tinystore_jobs.id = l.id";
const DROP_DEAD_LEASES: &str = "delete from _tinystore_jobs_leases";
/// A lease that ended without a settlement, of a job a write changes: its
/// attempt counts as a dead process's does.
const COUNT_ENDED_ATTEMPT: &str = "update _tinystore_jobs set attempt = l.attempt from _tinystore_jobs_leases l
    where l.id = ?1 and _tinystore_jobs.queue = l.queue and _tinystore_jobs.next = l.next and _tinystore_jobs.id = l.id";

const JOB_BY_KEY: &str = "select j.next, j.id, j.again, j.repeat, j.spill, j.expires
    from _tinystore_jobs_keys k join _tinystore_jobs j on j.queue = k.queue and j.next = k.next and j.id = k.id
    where k.queue = ?1 and k.key = ?2";
const LEASE_HELD: &str = "select 1 from _tinystore_jobs_leases where id = ?1 and until > ?2";
const DONE_KEY: &str = "select 1 from _tinystore_jobs_done where queue = ?1 and key = ?2 and until > ?3";

const INSERT_JOB: &str =
    "insert into _tinystore_jobs (queue, next, id, key, at, attempt, repeat, value, spill, grp, error, dedupe, keep,
        expires, latest, quiet)
    values (?1, ?2, ?3, ?4, ?2, 0, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)";
const INSERT_SPILLED: &str = "insert into _tinystore_jobs_spilled (id, value) values (?1, ?2)";
const DELETE_SPILLED: &str = "delete from _tinystore_jobs_spilled where id = ?1";
/// A key's row: where its job lies, replacing a key a job left behind.
const INSERT_KEY: &str = "insert into _tinystore_jobs_keys (queue, key, next, id) values (?1, ?2, ?3, ?4)
    on conflict (queue, key) do update set next = excluded.next, id = excluded.id";
const MOVE_KEY: &str = "update _tinystore_jobs_keys set next = ?3 where queue = ?1 and key = ?2";
const DROP_KEY: &str = "delete from _tinystore_jobs_keys where queue = ?1 and key = ?2";
/// A set's job made new: its value and time, its attempts from one, its
/// retry and error gone; the times of its last run stay, and so does a repeat,
/// a group or an option of its own the set does not name.
const RENEW_JOB: &str = "update _tinystore_jobs
    set next = ?4, at = ?4, attempt = 0, again = null, error = null, value = ?5, spill = ?6,
        latest = null, taken = null,
        repeat = coalesce(?7, repeat), grp = coalesce(?8, grp),
        dedupe = coalesce(?9, dedupe), keep = coalesce(?10, keep), expires = coalesce(?11, expires),
        quiet = coalesce(?12, quiet)
    where queue = ?1 and next = ?2 and id = ?3";
/// An update's job: its value, and what else the update named.
const UPDATE_JOB: &str = "update _tinystore_jobs
    set next = ?4, at = ?4, value = ?5, spill = ?6, repeat = coalesce(?7, repeat), grp = coalesce(?8, grp),
        dedupe = coalesce(?9, dedupe), keep = coalesce(?10, keep), expires = coalesce(?11, expires),
        quiet = coalesce(?12, quiet)
    where queue = ?1 and next = ?2 and id = ?3";
/// A set while the job runs: the value and repeat its next run takes, and when
/// that run is, which its settlement reads; no gather's latest time holds it.
const ASK_AGAIN: &str = "update _tinystore_jobs
    set again = ?4, value = ?5, spill = ?6, latest = null, taken = null,
        repeat = coalesce(?7, repeat), grp = coalesce(?8, grp),
        dedupe = coalesce(?9, dedupe), keep = coalesce(?10, keep), expires = coalesce(?11, expires),
        quiet = coalesce(?12, quiet)
    where queue = ?1 and next = ?2 and id = ?3";
/// What an upsert compares of the job that is there with its call: whether
/// its value is the call's, ?4, and its group and options of its own.
const SHAPE_OF: &str = "select
        coalesce(j.value, (select s.value from _tinystore_jobs_spilled s where s.id = j.spill)) is ?4,
        j.grp, j.dedupe, j.keep, j.expires, j.quiet
    from _tinystore_jobs j where j.queue = ?1 and j.next = ?2 and j.id = ?3";
/// A term an upsert renews, which changes nothing else.
const RENEW_TERM: &str = "update _tinystore_jobs set expires = ?4 where queue = ?1 and next = ?2 and id = ?3";
/// A job its term ended, with what it spilled; its key stays behind until
/// maintenance drops it, as a done job's does.
const END_TERM: &str = "delete from _tinystore_jobs where queue = ?1 and next = ?2 and id = ?3 returning spill";
/// An upsert's job: what the call changed, its time when its repeat changed,
/// and the time its run is for unless that time moved; its attempts as they
/// are. Each `next` on the right is the row's before the update.
const RESHAPE_JOB: &str = "update _tinystore_jobs
    set at = case when next = ?4 then at else ?4 end, next = ?4, value = ?5, spill = ?6,
        repeat = coalesce(?7, repeat), grp = coalesce(?8, grp),
        dedupe = coalesce(?9, dedupe), keep = coalesce(?10, keep), expires = coalesce(?11, expires),
        quiet = coalesce(?12, quiet)
    where queue = ?1 and next = ?2 and id = ?3";
/// A run asked of a job that runs, right after the run under way: its
/// settlement reads `again`.
const RUN_AFTER: &str = "update _tinystore_jobs set again = min(coalesce(again, ?4), ?4)
    where queue = ?1 and next = ?2 and id = ?3";
/// A job due now: one waiting for its time runs for now, and one waiting for
/// a retry runs that retry, for the time it was for.
const DUE_NOW: &str = "update _tinystore_jobs set next = ?4, at = case when attempt = 0 then ?4 else at end
    where queue = ?1 and next = ?2 and id = ?3";
/// The job under an id as a gather finds it, in one read: its row, its value
/// wherever it lies, the latest time its gathering runs, and whether a live
/// lease holds it.
const GATHERING: &str = "select j.next, j.id, j.again, j.repeat, j.spill, j.expires,
        coalesce(j.value, (select s.value from _tinystore_jobs_spilled s where s.id = j.spill)),
        j.latest, j.taken, j.quiet,
        exists (select 1 from _tinystore_jobs_leases l where l.id = j.id and l.until > ?3)
    from _tinystore_jobs_keys k join _tinystore_jobs j on j.queue = k.queue and j.next = k.next and j.id = k.id
    where k.queue = ?1 and k.key = ?2";
/// A gather's waiting job: its list, its time, and what else it named.
const GATHER_JOB: &str = "update _tinystore_jobs
    set next = ?4, at = ?4, value = ?5, spill = ?6, grp = coalesce(?7, grp),
        dedupe = coalesce(?8, dedupe), keep = coalesce(?9, keep), expires = coalesce(?10, expires), latest = ?11,
        taken = ?12, quiet = coalesce(?13, quiet)
    where queue = ?1 and next = ?2 and id = ?3";
/// A gather while the job runs: the whole list, its run's items first, which
/// `taken` counts, and when the job runs again, which the settlement reads
/// from `again`.
const GATHER_AGAIN: &str = "update _tinystore_jobs
    set again = ?4, value = ?5, spill = ?6, grp = coalesce(?7, grp),
        dedupe = coalesce(?8, dedupe), keep = coalesce(?9, keep), expires = coalesce(?10, expires), latest = ?11,
        taken = ?12, quiet = coalesce(?13, quiet)
    where queue = ?1 and next = ?2 and id = ?3";
/// A job's value wherever it lies.
const VALUE_OF: &str = "select coalesce(j.value, (select s.value from _tinystore_jobs_spilled s where s.id = j.spill))
    from _tinystore_jobs j where j.queue = ?1 and j.next = ?2 and j.id = ?3";
const SET_VALUE: &str = "update _tinystore_jobs set value = ?4, spill = ?5 where queue = ?1 and next = ?2 and id = ?3";
/// A job made from one that is there, with its id, group and options, due at
/// ?4 under the number ?5 with the value ?6: what was gathered while the one
/// there ran, which fails for good.
const FORK_JOB: &str =
    "insert into _tinystore_jobs (queue, next, id, key, at, attempt, repeat, value, spill, grp, error,
        dedupe, keep, expires, latest, quiet)
    select queue, ?4, ?5, key, ?4, 0, repeat, ?6, ?7, grp, ?8, dedupe, keep, expires, latest, quiet
    from _tinystore_jobs where queue = ?1 and next = ?2 and id = ?3
    returning key";
const DROP_FAILED: &str =
    "delete from _tinystore_jobs_failed where queue = ?1 and key = ?2 returning spill, grp, error";
const CANCEL_JOB: &str = "delete from _tinystore_jobs where (queue, next, id) in (
        select k.queue, k.next, k.id from _tinystore_jobs_keys k where k.queue = ?1 and k.key = ?2)
    returning id, spill, next, grp";
/// A job without an id by its number, due at ?2 or parked from it, ?3.
const CANCEL_ROW: &str = "delete from _tinystore_jobs
    where queue = ?1 and next in (?2, ?3) and id = ?4 and key is null
    returning id, spill, next, grp";
const DROP_FAILED_ROW: &str = "delete from _tinystore_jobs_failed
    where queue = ?1 and id = ?2 and failed = ?3 and key is null
    returning spill";
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
    /// When it leaves its queue unless written again.
    pub(crate) expires: Option<i64>,
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
    /// Why the failed job it takes the place of failed, until its first run ends.
    pub(crate) error: Option<&'a str>,
    pub(crate) own: Own<'a>,
    /// The latest it runs, when a gather started it with a max.
    pub(crate) latest: Option<i64>,
    pub(crate) value: &'a Kept,
}

/// What a gather finds of the job under its id beside its row.
#[derive(Debug)]
pub(crate) struct Gathered {
    pub(crate) value: Vec<u8>,
    pub(crate) latest: Option<i64>,
    /// The leading items of its list its run read, once a gather came while it ran.
    pub(crate) taken: Option<i64>,
    /// The job's own quiet hours, as it keeps them.
    pub(crate) quiet: Option<String>,
    /// Whether a live lease holds it: it runs.
    pub(crate) held: bool,
}

/// The options a job may have of its own beside its queue's, as a write gives
/// them; one it leaves out is its queue's, or stays as it was.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Own<'a> {
    /// How long it holds its id once done, as [`Dedupe::text`](super::dedupe::Dedupe) writes it.
    pub(crate) dedupe: Option<&'a str>,
    /// How long it is kept once it failed for good, in milliseconds; zero not at all.
    pub(crate) keep: Option<i64>,
    /// When it leaves its queue unless written again, unix milliseconds.
    pub(crate) expires: Option<i64>,
    /// Its quiet hours, as [`Quiet`](super::quiet::Quiet) keeps them: `none` holds it by none.
    pub(crate) quiet: Option<&'a str>,
}

/// What an upsert finds of the job that is there, beside its row.
#[derive(Debug)]
pub(crate) struct Shape {
    pub(crate) same_value: bool,
    pub(crate) group: Option<String>,
    pub(crate) dedupe: Option<String>,
    pub(crate) keep: Option<i64>,
    pub(crate) expires: Option<i64>,
    pub(crate) quiet: Option<String>,
}

/// What a failed job a write took the place of leaves to the new one.
#[derive(Debug)]
pub(crate) struct Failed {
    pub(crate) group: Option<String>,
    pub(crate) error: Option<String>,
}

/// What a write gives a job that is there: its value, its time and repeat,
/// and its group.
#[derive(Debug)]
pub(crate) struct Change<'a> {
    pub(crate) next: i64,
    pub(crate) repeat: Option<&'a str>,
    pub(crate) group: Option<&'a str>,
    pub(crate) own: Own<'a>,
    pub(crate) value: &'a Kept,
}

/// The id and kind of the queue `name`, if it was registered.
#[cfg(any(feature = "replication", feature = "wire"))]
pub(crate) fn find_queue(c: &Connection, name: &str) -> Result<Option<(i64, String)>> {
    c.prepare_cached(QUEUE_NAMED)
        .and_then(|mut select| select.query_row([name], |row| Ok((row.get(0)?, row.get(1)?))).optional())
        .map_err(|error| sql_error("a queue's registration", error))
}

/// Registers a queue on its first open, and answers its id, its kind and the
/// group bound it was last opened with.
pub(crate) fn queue(c: &Connection, name: &str, kind: &str) -> Result<(i64, String, i64)> {
    let named = || {
        c.prepare_cached(QUEUE_NAMED)
            .and_then(|mut select| {
                select.query_row([name], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()
            })
            .map_err(|error| sql_error("a queue's registration", error))
    };
    // Found before it is made, so that a copy, which writes nothing, opens a
    // queue its home made.
    if let Some(found) = named()? {
        return Ok(found);
    }
    execute(c, REGISTER_QUEUE, params![name, kind], "a queue's registration")?;
    named()?.ok_or_else(|| crate::Error::internal("a queue registered and not found"))
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
    execute(c, KEEP_IN_GROUP, params![queue, is], "a queue's group bound").map(drop)
}

/// The next job id, taken by the write that inserts its row: it commits or
/// rolls back with the row, so that an id is never given twice, not after a
/// crash either, and no write waits for another to reserve it one.
pub(crate) fn next_id(c: &Connection) -> Result<i64> {
    execute(c, NEXT_ID, [], "the next job id")?;
    c.prepare_cached(IDS)
        .and_then(|mut select| select.query_row([], |row| row.get(0)))
        .map_err(|error| sql_error("the next job id", error))
}

pub(crate) fn end_dead_leases(c: &Connection) -> Result<()> {
    execute(c, COUNT_DEAD_ATTEMPTS, [], "the leases of a process that died")?;
    execute(c, DROP_DEAD_LEASES, [], "the leases of a process that died").map(drop)
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
                        expires: row.get(5)?,
                    })
                })
                .optional()
        })
        .map_err(|error| sql_error("the job under an id", error))
}

/// The job under `key` and what a gather made at `now` needs of it.
pub(crate) fn gathering(c: &Connection, queue: i64, key: &str, now: i64) -> Result<Option<(Row, Gathered)>> {
    c.prepare_cached(GATHERING)
        .and_then(|mut select| {
            select
                .query_row(params![queue, key, now], |row| {
                    let (next, id, again, repeat, spill, expires) =
                        (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?);
                    let kept = Gathered {
                        value: row.get(6)?,
                        latest: row.get(7)?,
                        taken: row.get(8)?,
                        quiet: row.get(9)?,
                        held: row.get(10)?,
                    };
                    Ok((Row { next, id, again, repeat, spill, expires }, kept))
                })
                .optional()
        })
        .map_err(|error| sql_error("the job a gather finds under its id", error))
}

/// Writes a gather's list over the job that is there: a waiting one moves to
/// `change.next`, its steps gone with the list they were kept for; a running
/// one keeps its row and its run's steps, and its next run, which its
/// settlement starts at `change.next`, takes the list.
pub(crate) fn gather(
    c: &Connection,
    (queue, key): (i64, &str),
    there: &Row,
    (latest, taken, running): (Option<i64>, Option<i64>, bool),
    change: &Change<'_>,
) -> Result<()> {
    if !running {
        end_lease(c, there.id)?;
    }
    drop_spill(c, there.spill)?;
    let (spill, own) = (spill(c, there.id, change.value)?, change.own);
    let (inline, group) = (change.value.inline(), change.group);
    let params = params![
        queue,
        there.next,
        there.id,
        change.next,
        inline,
        spill,
        group,
        own.dedupe,
        own.keep,
        own.expires,
        latest,
        taken,
        own.quiet
    ];
    if running {
        return execute(c, GATHER_AGAIN, params, "a gather into a running job").map(drop);
    }
    execute(c, GATHER_JOB, params, "a gather's job")?;
    execute(c, DROP_STEPS, [there.id], "a gather's job: the steps kept for its old list")?;
    follow(c, queue, key, there.next, change.next)
}

/// The value of the job at `next` numbered `id`, wherever it lies.
pub(crate) fn value_of(c: &Connection, queue: i64, (next, id): (i64, i64)) -> Result<Vec<u8>> {
    c.prepare_cached(VALUE_OF)
        .and_then(|mut select| select.query_row(params![queue, next, id], |row| row.get(0)))
        .map_err(|error| sql_error("a job's value", error))
}

/// Gives the job at `next` numbered `id` this value, spilled anew past the
/// inline bound under the job's number.
pub(crate) fn set_value(c: &Connection, queue: i64, (next, id): (i64, i64), value: &Kept) -> Result<()> {
    drop_spill(c, Some(id))?;
    let spill = spill(c, id, value)?;
    execute(c, SET_VALUE, params![queue, next, id, value.inline(), spill], "a job's value").map(drop)
}

/// Adds a job beside the one at `next` numbered `id`, with its id, group and
/// options, due at `at` under the number `number`, holding `value` and the
/// error `error`; the id's key moves to it.
pub(crate) fn fork(
    c: &Connection,
    queue: i64,
    (next, id): (i64, i64),
    (at, number): (i64, i64),
    (value, error): (&Kept, &str),
) -> Result<()> {
    let spill = spill(c, number, value)?;
    let forked = params![queue, next, id, at, number, value.inline(), spill, error];
    let key: Option<String> = c
        .prepare_cached(FORK_JOB)
        .and_then(|mut insert| insert.query_row(forked, |row| row.get(0)))
        .map_err(|error| sql_error("a job made of what was gathered while it ran", error))?;
    if let Some(key) = key {
        execute(c, INSERT_KEY, params![queue, key, at, number], "a job's id")?;
    }
    Ok(())
}

/// Whether a claim holds the job at `now`; a lease that has ended holds
/// nothing, and the next claim takes the job.
pub(crate) fn lease_held(c: &Connection, id: i64, now: i64) -> Result<bool> {
    exists(c, LEASE_HELD, params![id, now])
}

pub(crate) fn done_key(c: &Connection, queue: i64, key: &str, now: i64) -> Result<bool> {
    exists(c, DONE_KEY, params![queue, key, now])
}

/// Refuses a job past the queue's `max_waiting`. The count is the queue's row,
/// which triggers keep, so that a transaction sees its own adds.
pub(crate) fn check_room(c: &Connection, queue: i64, max_waiting: u64) -> Result<()> {
    let waiting: i64 = c
        .prepare_cached(COUNT_WAITING)
        .and_then(|mut select| select.query_row([queue], |row| row.get(0)))
        .map_err(|error| sql_error("the jobs a queue holds", error))?;
    let waiting = u64::try_from(waiting).unwrap_or(0);
    if waiting >= max_waiting {
        let held = Limit { name: "waiting jobs", bound: max_waiting };
        return Err(held.refuse(waiting + 1, format!("the queue holds {waiting} jobs, its max_waiting")));
    }
    Ok(())
}

/// Adds a job's row, its value spilled to a row of its own past the inline
/// bound, and its key where it has one.
pub(crate) fn insert(c: &Connection, job: &NewJob<'_>) -> Result<()> {
    let spill = spill(c, job.id, job.value)?;
    let (inline, own) = (job.value.inline(), job.own);
    let params = params![
        job.queue,
        job.at,
        job.id,
        job.key,
        job.repeat,
        inline,
        spill,
        job.group,
        job.error,
        own.dedupe,
        own.keep,
        own.expires,
        job.latest,
        own.quiet
    ];
    execute(c, INSERT_JOB, params, "a job's row")?;
    if let Some(key) = job.key {
        execute(c, INSERT_KEY, params![job.queue, key, job.at, job.id], "a job's id")?;
    }
    Ok(())
}

/// Makes the job a set names new: its value, time, repeat and group, its
/// attempts from one, its steps gone with the run they belonged to. No live
/// lease holds it.
pub(crate) fn renew(c: &Connection, queue: i64, key: &str, there: &Row, change: &Change<'_>) -> Result<()> {
    end_lease(c, there.id)?;
    rewrite(c, queue, there, (Some(change.next), change), (RENEW_JOB, "a set's job"))?;
    execute(c, DROP_STEPS, [there.id], "a set's job: the steps of its last run")?;
    follow(c, queue, key, there.next, change.next)
}

/// Gives a job that is not running what an update names; a repeat or a group
/// left out stays as it was. No live lease holds it. The steps an earlier
/// attempt kept go with the value they were kept for, as a set's do.
pub(crate) fn update(c: &Connection, queue: i64, key: &str, there: &Row, change: &Change<'_>) -> Result<()> {
    end_lease(c, there.id)?;
    rewrite(c, queue, there, (Some(change.next), change), (UPDATE_JOB, "an update's job"))?;
    execute(c, DROP_STEPS, [there.id], "an update's job: the steps kept for its old value")?;
    follow(c, queue, key, there.next, change.next)
}

/// The job that is there as an upsert compares it with `value`.
pub(crate) fn shape(c: &Connection, queue: i64, there: &Row, value: &Kept) -> Result<Shape> {
    c.prepare_cached(SHAPE_OF)
        .and_then(|mut select| {
            select.query_row(params![queue, there.next, there.id, value.bytes()], |row| {
                let (same_value, group, dedupe) = (row.get(0)?, row.get(1)?, row.get(2)?);
                let (keep, expires, quiet) = (row.get(3)?, row.get(4)?, row.get(5)?);
                Ok(Shape { same_value, group, dedupe, keep, expires, quiet })
            })
        })
        .map_err(|error| sql_error("an upsert's job", error))
}

/// Brings a job that is not running to what an upsert names, its attempts as
/// they are; the steps an earlier attempt kept go only with a value they
/// were kept for. No live lease holds it.
pub(crate) fn reshape(
    c: &Connection,
    (queue, key): (i64, &str),
    there: &Row,
    change: &Change<'_>,
    value_changed: bool,
) -> Result<()> {
    end_lease(c, there.id)?;
    rewrite(c, queue, there, (Some(change.next), change), (RESHAPE_JOB, "an upsert's job"))?;
    if value_changed {
        execute(c, DROP_STEPS, [there.id], "an upsert's job: the steps kept for its old value")?;
    }
    follow(c, queue, key, there.next, change.next)
}

/// Ends the lease of a job whose lease ended before a write changed it, its
/// attempt counted: its holder may still settle, and would settle the job as
/// the write left it, deleting a change it was never given.
fn end_lease(c: &Connection, id: i64) -> Result<()> {
    execute(c, COUNT_ENDED_ATTEMPT, [id], "the lease a changed job outlived")?;
    execute(c, DROP_LEASE, [id], "the lease a changed job outlived").map(drop)
}

/// Writes what a change names over the job that is there with `statement`,
/// its value spilled anew, `time` its fourth parameter: when the job is due,
/// or when a running one runs again; `what` names the write in an error.
fn rewrite(
    c: &Connection,
    queue: i64,
    there: &Row,
    (time, change): (Option<i64>, &Change<'_>),
    (statement, what): (&str, &str),
) -> Result<()> {
    drop_spill(c, there.spill)?;
    let (spill, own) = (spill(c, there.id, change.value)?, change.own);
    let (inline, repeat, group) = (change.value.inline(), change.repeat, change.group);
    let own = (own.dedupe, own.keep, own.expires, own.quiet);
    let params = params![queue, there.next, there.id, time, inline, spill, repeat, group, own.0, own.1, own.2, own.3];
    execute(c, statement, params, what).map(drop)
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
    rewrite(c, queue, there, (again, change), (ASK_AGAIN, "a set of a running job"))
}

/// Writes a job's renewed term alone.
pub(crate) fn renew_term(c: &Connection, queue: i64, there: &Row, expires: i64) -> Result<()> {
    execute(c, RENEW_TERM, params![queue, there.next, there.id, expires], "a job's renewed term").map(drop)
}

/// Deletes a job whose term ended, no run holding it, with what it spilled
/// and the steps its triggers take; its key stays behind for maintenance.
pub(crate) fn end_term(c: &Connection, queue: i64, there: &Row) -> Result<()> {
    end_job(c, queue, (there.next, there.id))
}

/// Deletes the job at `next` numbered `id` and its spilled value.
pub(crate) fn end_job(c: &Connection, queue: i64, (next, id): (i64, i64)) -> Result<()> {
    let spilled: Option<Option<i64>> = c
        .prepare_cached(END_TERM)
        .and_then(|mut delete| delete.query_row(params![queue, next, id], |row| row.get(0)).optional())
        .map_err(|error| sql_error("a job its term ended", error))?;
    execute(c, DROP_LEASE, [id], "a job its term ended: its lease")?;
    drop_spill(c, spilled.flatten())
}

/// Asks one run more of a job that runs, at `at` or as soon as the run under
/// way ends, its value and repeat as they are.
pub(crate) fn run_after(c: &Connection, queue: i64, there: &Row, at: i64) -> Result<()> {
    execute(c, RUN_AFTER, params![queue, there.next, there.id, at], "a run asked of a running job").map(drop)
}

/// Makes a job that no live lease holds due at `now`, its attempts as they are.
pub(crate) fn due_now(c: &Connection, queue: i64, key: &str, there: &Row, now: i64) -> Result<()> {
    end_lease(c, there.id)?;
    execute(c, DUE_NOW, params![queue, there.next, there.id, now], "a job made due now")?;
    follow(c, queue, key, there.next, now)
}

/// Deletes the failed job under a key and its spilled value, and answers
/// its group and its error when there was one.
pub(crate) fn drop_failed(c: &Connection, queue: i64, key: &str) -> Result<Option<Failed>> {
    let dropped: Option<(Option<i64>, Failed)> = c
        .prepare_cached(DROP_FAILED)
        .and_then(|mut delete| {
            let failed = |row: &rusqlite::Row<'_>| Ok((row.get(0)?, Failed { group: row.get(1)?, error: row.get(2)? }));
            delete.query_row(params![queue, key], failed).optional()
        })
        .map_err(|error| sql_error("a failed job", error))?;
    let Some((spilled, failed)) = dropped else {
        return Ok(None);
    };
    drop_spill(c, spilled)?;
    Ok(Some(failed))
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
    let Some(taken) = take(c, CANCEL_JOB, params![queue, key])? else {
        return Ok(None);
    };
    execute(c, DROP_KEY, params![queue, key], "a cancel: the job's id")?;
    Ok(Some(taken))
}

/// Deletes the job without an id numbered `number` that is due at `due`,
/// waiting, parked or running, with its lease and its spilled value.
pub(crate) fn cancel_row(c: &Connection, queue: i64, due: i64, number: i64) -> Result<Option<Taken>> {
    take(c, CANCEL_ROW, params![queue, due, due.saturating_add(PARKED_BY), number])
}

/// Deletes the job row `delete` finds, then its lease and its spilled value.
fn take(c: &Connection, delete: &str, params: impl Params) -> Result<Option<Taken>> {
    let taken: Option<(i64, Option<i64>, i64, Option<String>)> = c
        .prepare_cached(delete)
        .and_then(|mut delete| {
            delete.query_row(params, |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).optional()
        })
        .map_err(|error| sql_error("a cancel", error))?;
    let Some((id, spilled, next, group)) = taken else {
        return Ok(None);
    };
    execute(c, DROP_LEASE, [id], "a cancel: the job's lease")?;
    drop_spill(c, spilled)?;
    Ok(Some(Taken { id, group, placed: next < PARKED_FROM }))
}

/// Deletes the failed job without an id numbered `number` that failed at
/// `failed`, and its spilled value; says whether there was one.
pub(crate) fn drop_failed_row(c: &Connection, queue: i64, failed: i64, number: i64) -> Result<bool> {
    let dropped: Option<Option<i64>> = c
        .prepare_cached(DROP_FAILED_ROW)
        .and_then(|mut delete| delete.query_row(params![queue, number, failed], |row| row.get(0)).optional())
        .map_err(|error| sql_error("a failed job", error))?;
    let Some(spilled) = dropped else {
        return Ok(false);
    };
    drop_spill(c, spilled)?;
    Ok(true)
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
    execute(c, MOVE_KEY, params![queue, key, to], "a job's id").map(drop)
}

pub(crate) fn drop_spill(c: &Connection, spilled: Option<i64>) -> Result<()> {
    match spilled {
        Some(id) => execute(c, DELETE_SPILLED, [id], "a large value").map(drop),
        None => Ok(()),
    }
}

pub(crate) fn drop_steps(c: &Connection, job: i64) -> Result<()> {
    execute(c, DROP_STEPS, [job], "the steps of a run that ended").map(drop)
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

pub(crate) use crate::sqlite::execute_cached as execute;

pub(crate) fn exists(c: &Connection, sql: &str, params: impl Params) -> Result<bool> {
    c.prepare_cached(sql).and_then(|mut select| select.exists(params)).map_err(|error| sql_error("a read", error))
}
