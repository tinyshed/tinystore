//! Claims and settlements: a claim leases due jobs to a worker, and a
//! settlement ends a lease by what its handler answered. A lease is its
//! attempt: a settlement names the attempt it claimed, and one whose lease
//! ended and whose job another claim has taken since changes nothing.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use rusqlite::{Connection, OptionalExtension, params};

use super::policy::Policy;
use super::repeat::Repeat;
use super::rows::{self, PARKED_BY, execute};
use super::state::QueueState;
use crate::sqlite::{query_cached, sql_error};
use crate::text::fnv1a;
use crate::{Error, ErrorKind, Result};

/// Jobs one claim takes at most, and jobs of full groups it parks at most,
/// so that a backlog holds the writer briefly.
pub(crate) const CLAIM_BATCH: usize = 1000;
const PARK_AT_MOST: usize = 1000;

/// A failure as a job keeps it: its text, at most this long.
const MAX_ERROR: usize = 4096;

const CLAIM_JOBS: &str = "select next, id, key, at, attempt, value, spill, grp
    from _tinystore_jobs j
    where queue = ?1 and next <= ?2
        and not exists (select 1 from _tinystore_jobs_leases l where l.id = j.id and l.until > ?2)
    order by next, id limit cast(?3 as integer)";
/// A lease's attempt counts the attempt a lease that ended without a
/// settlement held, so that a job whose process died each time fails for good.
const TAKE_LEASE: &str =
    "insert into _tinystore_jobs_leases (id, queue, next, attempt, until) values (?1, ?2, ?3, ?4, ?5)
    on conflict (id) do update set next = excluded.next, until = excluded.until,
        attempt = max(_tinystore_jobs_leases.attempt, excluded.attempt - 1) + 1
    returning attempt";
const LIVE_LEASES: &str = "select count(*) from _tinystore_jobs_leases where queue = ?1 and until > ?2";
const RUNNING_IN_GROUPS: &str = "select j.grp, count(*) from _tinystore_jobs_leases l
        join _tinystore_jobs j on j.queue = l.queue and j.next = l.next and j.id = l.id
    where l.queue = ?1 and l.until > ?2 and j.grp is not null
    group by j.grp";
const NEXT_DUE: &str = "select next from _tinystore_jobs j where queue = ?1 and next < 576460752303423488
        and not exists (select 1 from _tinystore_jobs_leases l where l.id = j.id and l.until > ?2)
    order by next, id limit 1";
const EARLIEST_LEASE: &str = "select min(until) from _tinystore_jobs_leases where queue = ?1 and until > ?2";

/// A settlement drops its lease only where its claim left it: a claim that
/// parks a row moves its lease along, and a write that changes a row no live
/// lease holds ends the lease first, so a lease found there finds its row
/// there too. One whose row moved since writes nothing, not even the drop of
/// its lease, and is lost.
const DROP_LEASE: &str = "delete from _tinystore_jobs_leases where id = ?1 and attempt = ?2 and next = ?3";
const DROP_ANY_LEASE: &str = "delete from _tinystore_jobs_leases where id = ?1";
const EXTEND_LEASE: &str = "update _tinystore_jobs_leases set until = ?3 where id = ?1 and attempt = ?2 and next = ?4";
const JOB_HELD: &str =
    "select again, repeat, error, attempt from _tinystore_jobs where queue = ?1 and next = ?2 and id = ?3";
const DELETE_JOB: &str = "delete from _tinystore_jobs where queue = ?1 and next = ?2 and id = ?3 returning spill";
const DELETE_DONE: &str = "delete from _tinystore_jobs where queue = ?1 and next = ?2 and id = ?3
    and again is null and repeat is null returning spill";
const MOVE_JOB: &str = "update _tinystore_jobs set next = ?4, at = ?5, attempt = ?6, again = null, error = ?7,
        ran = coalesce(?8, ran), took = coalesce(?9, took)
    where queue = ?1 and next = ?2 and id = ?3";
const FAIL_JOB: &str = "insert into _tinystore_jobs_failed (queue, id, key, at, attempt, failed, error, value, spill,
        ran, took, grp)
    select queue, id, key, at, ?4, ?5, ?6, value, spill, coalesce(?7, ran), coalesce(?8, took), grp
    from _tinystore_jobs where queue = ?1 and next = ?2 and id = ?3";
const FORGET_DONE: &str = "delete from _tinystore_jobs_done where queue = ?1 and key = ?2 returning spill";
const KEEP_DONE: &str =
    "insert into _tinystore_jobs_done (queue, key, until, at, attempt, value, spill, ran, took, grp)
    select queue, key, ?4, at, ?5, value, spill, coalesce(?6, ran), coalesce(?7, took), grp
    from _tinystore_jobs where queue = ?1 and next = ?2 and id = ?3";
const DROP_JOB_ROW: &str = "delete from _tinystore_jobs where queue = ?1 and next = ?2 and id = ?3";

/// A job in a worker's hands: where its row is, the attempt that is its token,
/// and what memory knows of its run.
#[derive(Debug)]
pub(crate) struct Lease {
    pub(crate) next: i64,
    pub(crate) id: i64,
    pub(crate) attempt: i64,
    pub(crate) at: i64,
    pub(crate) key: Option<String>,
    pub(crate) spill: Option<i64>,
    pub(crate) group: Option<String>,
    /// The value when the row held it; a spilled one is read outside the writer.
    pub(crate) inline: Option<Vec<u8>>,
    state: Mutex<LeaseState>,
    on_cancel: CancelHook,
}

/// What a cancel of the job tells whoever runs it beyond this process's
/// threads: a client's worker, whose handler stops on it.
#[derive(Default)]
struct CancelHook(Mutex<Option<Box<dyn FnOnce() + Send>>>);

impl std::fmt::Debug for CancelHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CancelHook")
    }
}

#[derive(Debug, Default)]
struct LeaseState {
    until: i64,
    /// When a handler began the job; none until one did.
    began: Option<i64>,
    settled: bool,
    /// Another claim, or a cancel, took the job after the lease ended.
    lost: bool,
    cancelled: bool,
    progress: Option<serde_json::Value>,
}

/// Where a job's row lies, its next and its number, and its id, whose row
/// follows it when it moves.
type Place<'a> = (i64, i64, Option<&'a str>);

impl Lease {
    pub(crate) fn until(&self) -> i64 {
        self.lock().until
    }

    fn place(&self) -> Place<'_> {
        (self.next, self.id, self.key.as_deref())
    }

    /// Says a handler began the job at `now`; false when a cancel took it first,
    /// and nothing may start it.
    pub(crate) fn begin(&self, now: i64) -> bool {
        let mut state = self.lock();
        if state.cancelled {
            return false;
        }
        state.began = Some(now);
        true
    }

    /// Whether a handler runs the job at `now`, rather than a worker keeping it
    /// for a handler still busy, or its lease having ended.
    pub(crate) fn running(&self, now: i64) -> bool {
        let state = self.lock();
        state.began.is_some() && !state.settled && state.until > now
    }

    pub(crate) fn started(&self) -> bool {
        self.lock().began.is_some()
    }

    /// Marks the lease of a job a cancel took: it settles nothing, and what
    /// runs it is told.
    pub(crate) fn cancel(&self) {
        let mut state = self.lock();
        (state.settled, state.lost, state.cancelled) = (true, true, true);
        drop(state);
        let hook = self.on_cancel.0.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(hook) = hook {
            hook();
        }
    }

    /// Calls `hook` when a cancel takes the job, at once when one has.
    #[cfg(feature = "wire")]
    pub(crate) fn when_cancelled(&self, hook: Box<dyn FnOnce() + Send>) {
        if self.cancelled() {
            return hook();
        }
        *self.on_cancel.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(hook);
        // a cancel between the check and the hook's keeping finds no hook
        if self.cancelled()
            && let Some(hook) = self.on_cancel.0.lock().unwrap_or_else(PoisonError::into_inner).take()
        {
            hook();
        }
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.lock().cancelled
    }

    pub(crate) fn settled(&self) -> bool {
        self.lock().settled
    }

    pub(crate) fn progress(&self) -> Option<serde_json::Value> {
        self.lock().progress.clone()
    }

    pub(crate) fn set_progress(&self, progress: serde_json::Value) {
        self.lock().progress = Some(progress);
    }

    /// The run a settlement at `now` records: when it began and how long it
    /// took; none when nothing began it.
    pub(crate) fn last_run(&self, now: i64) -> (Option<i64>, Option<i64>) {
        let began = self.lock().began;
        (began, began.map(|began| (now - began).max(0)))
    }

    /// Applies what a written settlement changed.
    pub(crate) fn apply(&self, settled: &Settled, extension: bool) {
        let mut state = self.lock();
        if let Some(until) = settled.until {
            state.until = until;
        }
        if settled.lost {
            (state.settled, state.lost) = (true, true);
        } else if !extension {
            state.settled = true;
        }
    }

    fn lock(&self) -> MutexGuard<'_, LeaseState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One claim: the queue it takes from, when, until when it leases, and how
/// many jobs it takes at most.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Claiming<'a> {
    pub(crate) queue: &'a QueueState,
    pub(crate) now: i64,
    pub(crate) until: i64,
    pub(crate) limit: usize,
}

/// What a claim leased, the jobs whose attempts had all ended without a
/// settlement, which it leased no more, and whether it stopped at its bound of
/// jobs parked with more due ones to look at.
#[derive(Debug, Default)]
pub(crate) struct Claimed {
    pub(crate) leases: Vec<Lease>,
    pub(crate) abandoned: Vec<i64>,
    /// Of the abandoned, those failed for good: the others repeat, or a set
    /// asked them to run again, and wait for that time.
    pub(crate) failed: usize,
    pub(crate) more: bool,
}

/// Leases up to the claim's limit of due jobs, leaving the values they spilled
/// for their workers to read outside the writer.
///
/// A job whose attempts all ended without a settlement, its process dead or
/// its worker gone each time, fails for good instead of running again.
///
/// A queue with a total takes no more than its room, the writer counting the
/// live leases so that no two claims both see the last place free; one with a
/// rate no more than the rate lets start.
pub(crate) fn claim(c: &Connection, claiming: Claiming<'_>) -> Result<Claimed> {
    let room = room(c, &claiming)?;
    let limit = match &claiming.queue.rate {
        Some(rate) => rate.take(claiming.now, room),
        None => room,
    };
    if limit == 0 {
        return Ok(Claimed::default());
    }
    let claiming = Claiming { limit, ..claiming };
    let grouped = claiming.queue.policy.concurrency.group.is_some() || claiming.queue.group_rates.is_some();
    let claimed = if grouped { by_group(c, claiming) } else { in_order(c, claiming) };
    if let Some(rate) = &claiming.queue.rate {
        rate.give_back(limit - claimed.as_ref().map_or(0, |claimed| claimed.leases.len()));
    }
    claimed
}

/// How many jobs the claim may lease: its limit, and for a queue with a total
/// no more than the places its live leases leave.
fn room(c: &Connection, claiming: &Claiming<'_>) -> Result<usize> {
    let Some(total) = claiming.queue.policy.concurrency.total else {
        return Ok(claiming.limit);
    };
    let held: i64 = c
        .prepare_cached(LIVE_LEASES)
        .and_then(|mut select| select.query_row(params![claiming.queue.id, claiming.now], |row| row.get(0)))
        .map_err(|error| sql_error("a claim: the jobs running", error))?;
    let free = i64::from(total).saturating_sub(held).max(0);
    Ok(claiming.limit.min(usize::try_from(free).unwrap_or(0)))
}

/// Leases the first due jobs in the order of their time.
fn in_order(c: &Connection, claiming: Claiming<'_>) -> Result<Claimed> {
    let mut claimed = Claimed::default();
    for row in due(c, &claiming, claiming.limit)? {
        claimed.lease(c, &claiming, row)?;
    }
    Ok(claimed)
}

/// Leases due jobs in the order of their time, as `in_order` does, and moves
/// out of the way those of a group that may start none: parked while its
/// places are taken, or set to its turn under its group rate, counting what
/// it leases. Past `PARK_AT_MOST` moved it stops, with more due jobs to look
/// at.
fn by_group(c: &Connection, claiming: Claiming<'_>) -> Result<Claimed> {
    let bound = i64::from(claiming.queue.policy.concurrency.group.unwrap_or(u32::MAX));
    let mut running = running_by_group(c, &claiming)?;
    let mut claimed = Claimed::default();
    let (mut moved, mut waiting) = (0, HashMap::new());
    while claimed.leases.len() < claiming.limit {
        let asked = claiming.limit - claimed.leases.len();
        let due_now = due(c, &claiming, asked)?;
        let found = due_now.len();
        for row in due_now {
            let full = row.group.as_ref().is_some_and(|group| running.get(group).copied().unwrap_or(0) >= bound);
            let to = match (&row.group, &claiming.queue.group_rates) {
                _ if full => Some(row.next + PARKED_BY),
                (Some(group), Some(rates)) => {
                    let behind = waiting.entry(group.clone()).or_insert(0);
                    let turn = rates.take(group, claiming.now, *behind).err();
                    *behind += usize::from(turn.is_some());
                    turn
                }
                _ => None,
            };
            if let Some(to) = to {
                if moved == PARK_AT_MOST {
                    claimed.more = true;
                    return Ok(claimed);
                }
                moved += 1;
                rows::move_to(c, claiming.queue.id, (row.next, row.id, row.key.as_deref()), to)?;
                continue;
            }
            claimed.lease_in_group(c, &claiming, row, &mut running)?;
        }
        if found < asked {
            break;
        }
    }
    Ok(claimed)
}

fn running_by_group(c: &Connection, claiming: &Claiming<'_>) -> Result<HashMap<String, i64>> {
    let read = (RUNNING_IN_GROUPS, "a claim: the jobs each group runs");
    query_cached(c, read, params![claiming.queue.id, claiming.now], |row| Ok((row.get(0)?, row.get(1)?)))
}

/// A due job as a claim reads it.
#[derive(Debug)]
struct DueRow {
    next: i64,
    id: i64,
    key: Option<String>,
    at: i64,
    attempt: i64,
    value: Option<Vec<u8>>,
    spill: Option<i64>,
    group: Option<String>,
}

/// Up to `limit` due jobs no live lease holds, in the order of their time.
fn due(c: &Connection, claiming: &Claiming<'_>, limit: usize) -> Result<Vec<DueRow>> {
    let read = (CLAIM_JOBS, "a claim: the jobs due");
    query_cached(c, read, params![claiming.queue.id, claiming.now, limit as i64], |row| {
        Ok(DueRow {
            next: row.get(0)?,
            id: row.get(1)?,
            key: row.get(2)?,
            at: row.get(3)?,
            attempt: row.get(4)?,
            value: row.get(5)?,
            spill: row.get(6)?,
            group: row.get(7)?,
        })
    })
}

impl Claimed {
    /// Leases a due row, or fails it for good when this would be an attempt
    /// past the queue's attempts, in which case a job of its group may take the
    /// place it no longer wants. Says whether it leased.
    fn lease(&mut self, c: &Connection, claiming: &Claiming<'_>, row: DueRow) -> Result<bool> {
        let queue = claiming.queue;
        let attempt: i64 = c
            .prepare_cached(TAKE_LEASE)
            .and_then(|mut insert| {
                insert.query_row(params![row.id, queue.id, row.next, row.attempt + 1, claiming.until], |found| {
                    found.get(0)
                })
            })
            .map_err(|error| sql_error("a claim: a job's lease", error))?;
        if attempt > i64::from(queue.policy.attempts) {
            if abandon(c, claiming, &row, attempt - 1)? {
                self.failed += 1;
            }
            self.abandoned.push(row.id);
            if let (Some(group), Some(_)) = (&row.group, queue.policy.concurrency.group) {
                rows::unpark_one(c, queue.id, group)?;
            }
            return Ok(false);
        }
        let state = LeaseState { until: claiming.until, ..LeaseState::default() };
        self.leases.push(Lease {
            next: row.next,
            id: row.id,
            attempt,
            at: row.at,
            key: row.key,
            spill: row.spill,
            group: row.group,
            inline: row.value,
            state: Mutex::new(state),
            on_cancel: CancelHook::default(),
        });
        Ok(true)
    }

    /// Leases a due job of a group, counting it among the group's running
    /// jobs; one it leases no more gives back the start its group rate
    /// counted for it.
    fn lease_in_group(
        &mut self,
        c: &Connection,
        claiming: &Claiming<'_>,
        row: DueRow,
        running: &mut HashMap<String, i64>,
    ) -> Result<()> {
        let group = row.group.clone();
        let leased = self.lease(c, claiming, row)?;
        let Some(group) = group else { return Ok(()) };
        match (leased, &claiming.queue.group_rates) {
            (true, _) => *running.entry(group).or_default() += 1,
            (false, Some(rates)) => rates.give_back(&group),
            (false, None) => {}
        }
        Ok(())
    }
}

/// Ends a job whose every attempt ended without a settlement, as a run that
/// failed ends: one that repeats, or that a set asked to run again, waits for
/// that time, and any other fails for good. Says whether it failed.
fn abandon(c: &Connection, claiming: &Claiming<'_>, row: &DueRow, attempts: i64) -> Result<bool> {
    let cause = format!(
        "each of its {attempts} attempts ended without a settlement: its process died or its worker vanished while it ran"
    );
    let (queue, place) = (claiming.queue, (row.next, row.id, row.key.as_deref()));
    execute(c, DROP_ANY_LEASE, [row.id], "an abandoned job's lease")?;
    if let Some(next) = next_run(row.at, &held(c, queue, place)?, claiming.now) {
        run_again(c, queue, place, (next, Some(cause)), (None, None))?;
        return Ok(false);
    }
    let failed =
        params![queue.id, row.next, row.id, attempts, claiming.now, cause, Option::<i64>::None, Option::<i64>::None];
    execute(c, FAIL_JOB, failed, "an abandoned job")?;
    execute(c, DROP_JOB_ROW, params![queue.id, row.next, row.id], "an abandoned job")?;
    Ok(true)
}

/// When the queue next needs a claim: its first due job no live lease holds,
/// or the first live lease's end; none when neither is. For a queue whose total
/// is taken, the first live lease's end alone, since only that or a
/// settlement, which wakes the workers itself, makes room.
pub(crate) fn next_claim(c: &Connection, claiming: &Claiming<'_>) -> Result<Option<i64>> {
    let full = claiming.queue.policy.concurrency.total.is_some() && room(c, &Claiming { limit: 1, ..*claiming })? == 0;
    let earliest_lease: Option<i64> = c
        .prepare_cached(EARLIEST_LEASE)
        .and_then(|mut select| select.query_row(params![claiming.queue.id, claiming.now], |row| row.get(0)))
        .map_err(|error| sql_error("a claim: the next lease to end", error))?;
    if full {
        return Ok(earliest_lease);
    }
    let due: Option<i64> = c
        .prepare_cached(NEXT_DUE)
        .and_then(|mut select| select.query_row(params![claiming.queue.id, claiming.now], |row| row.get(0)).optional())
        .map_err(|error| sql_error("a claim: the next job due", error))?;
    Ok(match (due, earliest_lease) {
        (Some(due), Some(lease)) => Some(due.min(lease)),
        (due, lease) => due.or(lease),
    })
}

/// How a settlement ends a lease.
#[derive(Clone, Debug)]
pub(crate) enum How {
    Done,
    /// An attempt failed: again at `at`, or after the queue's backoff, and past
    /// its attempts it fails for good.
    Retry {
        at: Option<i64>,
        cause: String,
    },
    Fail {
        cause: String,
    },
    /// Not yet: again at `at`, the attempt not counted.
    Snooze {
        at: i64,
    },
    /// A worker that stopped before it ran the job: due at once, the attempt
    /// not counted.
    GiveBack,
    Extend {
        until: i64,
    },
}

/// What a settlement changed that memory follows once it commits.
#[derive(Debug, Default)]
pub(crate) struct Settled {
    pub(crate) failed: Option<String>,
    /// When it is due again.
    pub(crate) due: Option<i64>,
    /// The lease was no longer the settlement's: nothing was written.
    pub(crate) lost: bool,
    pub(crate) until: Option<i64>,
}

/// Writes one settlement. A lease that is no longer this worker's writes
/// nothing and answers `lost`.
pub(crate) fn settle(c: &Connection, queue: &QueueState, lease: &Lease, how: &How, now: i64) -> Result<Settled> {
    match write(c, queue, lease, how, now) {
        Err(error) if error.kind() == ErrorKind::Conflict => Ok(Settled { lost: true, ..Settled::default() }),
        settled => settled,
    }
}

fn write(c: &Connection, queue: &QueueState, lease: &Lease, how: &How, now: i64) -> Result<Settled> {
    if let How::Extend { until } = how {
        changed_one(c, EXTEND_LEASE, params![lease.id, lease.attempt, until, lease.next])?;
        return Ok(Settled { until: Some(*until), ..Settled::default() });
    }
    changed_one(c, DROP_LEASE, params![lease.id, lease.attempt, lease.next])?;
    if !matches!(how, How::GiveBack)
        && let (Some(group), Some(_)) = (&lease.group, queue.policy.concurrency.group)
    {
        rows::unpark_one(c, queue.id, group)?;
    }
    if matches!(how, How::Done)
        && let Some(settled) = done_alone(c, queue, lease)?
    {
        return Ok(settled);
    }
    let held = held(c, queue, lease.place())?;
    if let Some(settled) = superseded(c, queue, lease, (&held, how), now)? {
        return Ok(settled);
    }
    let ran = lease.last_run(now);
    match how {
        How::Done => done(c, queue, lease, &held, now),
        How::Retry { at, cause } => retry(c, queue, lease, &held, (*at, cause), now),
        How::Fail { cause } => fail(c, queue, lease, &held, cause, now),
        How::Snooze { at } => {
            let to = Moved { next: *at, at: lease.at, attempt: held.attempt, error: held.error.clone(), ran };
            move_job(c, queue, lease.place(), &to)?;
            Ok(Settled { due: Some(*at), ..Settled::default() })
        }
        How::GiveBack => {
            let to = Moved {
                next: lease.next,
                at: lease.at,
                attempt: held.attempt,
                error: held.error.clone(),
                ran: (None, None),
            };
            move_job(c, queue, lease.place(), &to)?;
            Ok(Settled { due: Some(now), ..Settled::default() })
        }
        How::Extend { .. } => unreachable!("an extension returned above"),
    }
}

/// Starts the run a set asked for while the job ran, where the run under way
/// would otherwise go on: after a retry, a snooze or a give back. Going on, it
/// would read the set's value, its steps answering with what they kept for
/// the value before, and run at its own time rather than the set's. A done
/// or a fail ends the run, and starts the set's by itself.
fn superseded(
    c: &Connection,
    queue: &QueueState,
    lease: &Lease,
    (held, how): (&HeldRow, &How),
    now: i64,
) -> Result<Option<Settled>> {
    let error = match how {
        How::Retry { cause, .. } => Some(bounded(cause)),
        How::Snooze { .. } | How::GiveBack => None,
        How::Done | How::Fail { .. } | How::Extend { .. } => return Ok(None),
    };
    match (held.again, next_run(lease.at, held, now)) {
        (Some(_), Some(next)) => run_again(c, queue, lease.place(), (next, error), lease.last_run(now)).map(Some),
        _ => Ok(None),
    }
}

/// Deletes a done job that neither repeats nor was asked to run again,
/// reading nothing first; none for a job that moves to its next time instead,
/// or one whose id the queue's dedupe keeps.
fn done_alone(c: &Connection, queue: &QueueState, lease: &Lease) -> Result<Option<Settled>> {
    if queue.policy.dedupe.is_some() && lease.key.is_some() {
        return Ok(None);
    }
    let spilled: Option<Option<i64>> = c
        .prepare_cached(DELETE_DONE)
        .and_then(|mut delete| delete.query_row(params![queue.id, lease.next, lease.id], |row| row.get(0)).optional())
        .map_err(|error| sql_error("a done job", error))?;
    let Some(spilled) = spilled else {
        return Ok(None);
    };
    rows::drop_spill(c, spilled)?;
    Ok(Some(Settled::default()))
}

/// What a settlement needs of the job's row.
#[derive(Debug)]
struct HeldRow {
    again: Option<i64>,
    repeat: Option<String>,
    error: Option<String>,
    attempt: i64,
}

fn held(c: &Connection, queue: &QueueState, (next, id, _): Place<'_>) -> Result<HeldRow> {
    c.prepare_cached(JOB_HELD)
        .and_then(|mut select| {
            select
                .query_row(params![queue.id, next, id], |row| {
                    Ok(HeldRow { again: row.get(0)?, repeat: row.get(1)?, error: row.get(2)?, attempt: row.get(3)? })
                })
                .optional()
        })
        .map_err(|error| sql_error("a settled job's row", error))?
        .ok_or_else(lost)
}

/// Deletes a done job, or moves one that repeats, or that a set asked to run
/// again, to its next time; a job whose id the queue's dedupe keeps leaves its
/// value and last run behind with the id.
fn done(c: &Connection, queue: &QueueState, lease: &Lease, held: &HeldRow, now: i64) -> Result<Settled> {
    if let Some(next) = next_run(lease.at, held, now) {
        return run_again(c, queue, lease.place(), (next, None), lease.last_run(now));
    }
    if let (Some(dedupe), Some(key)) = (queue.policy.dedupe, &lease.key) {
        let forgotten: Option<Option<i64>> = c
            .prepare_cached(FORGET_DONE)
            .and_then(|mut delete| delete.query_row(params![queue.id, key], |row| row.get(0)).optional())
            .map_err(|error| sql_error("a done job's id", error))?;
        rows::drop_spill(c, forgotten.flatten())?;
        let until = now.saturating_add(crate::clock::millis(dedupe));
        let (ran, took) = lease.last_run(now);
        let keep = params![queue.id, lease.next, lease.id, until, lease.attempt, ran, took];
        execute(c, KEEP_DONE, keep, "a done job's id")?;
        execute(c, DROP_JOB_ROW, params![queue.id, lease.next, lease.id], "a done job")?;
        return Ok(Settled::default());
    }
    let spilled: Option<i64> = c
        .prepare_cached(DELETE_JOB)
        .and_then(|mut delete| delete.query_row(params![queue.id, lease.next, lease.id], |row| row.get(0)))
        .map_err(|error| sql_error("a done job", error))?;
    rows::drop_spill(c, spilled)?;
    Ok(Settled::default())
}

/// Moves the job to its next attempt's time, or fails it past its attempts.
fn retry(
    c: &Connection,
    queue: &QueueState,
    lease: &Lease,
    held: &HeldRow,
    (at, cause): (Option<i64>, &str),
    now: i64,
) -> Result<Settled> {
    if lease.attempt >= i64::from(queue.policy.attempts) {
        return fail(c, queue, lease, held, cause, now);
    }
    let mut due = at.unwrap_or_else(|| now.saturating_add(backoff(&queue.policy, lease.attempt, lease.id)));
    if let Some(next) = next_run(lease.at, held, now) {
        due = due.min(next);
    }
    let to = Moved {
        next: due,
        at: lease.at,
        attempt: lease.attempt,
        error: Some(bounded(cause)),
        ran: lease.last_run(now),
    };
    move_job(c, queue, lease.place(), &to)?;
    Ok(Settled { due: Some(due), ..Settled::default() })
}

/// Keeps the job as failed for good. A job that repeats, or that a set asked
/// to run again, waits for that time instead, its error kept.
fn fail(c: &Connection, queue: &QueueState, lease: &Lease, held: &HeldRow, cause: &str, now: i64) -> Result<Settled> {
    let cause = bounded(cause);
    if let Some(next) = next_run(lease.at, held, now) {
        return run_again(c, queue, lease.place(), (next, Some(cause)), lease.last_run(now));
    }
    let (ran, took) = lease.last_run(now);
    let failed = params![queue.id, lease.next, lease.id, lease.attempt, now, cause, ran, took];
    execute(c, FAIL_JOB, failed, "a failed job")?;
    execute(c, DROP_JOB_ROW, params![queue.id, lease.next, lease.id], "a failed job")?;
    Ok(Settled { failed: Some(cause), ..Settled::default() })
}

/// When a settled job, whose run was for `at`, runs again without a retry: the
/// earlier of its repeat's next time, after `at` and after now, and the time a
/// set asked of it.
fn next_run(at: i64, held: &HeldRow, now: i64) -> Option<i64> {
    let repeated = held.repeat.as_deref().and_then(|text| match Repeat::parse(text) {
        Ok(repeat) => repeat.next(at.max(now)),
        Err(error) => {
            tracing::error!(target: "tinystore", %error, "a repeating job's repeat does not read");
            None
        }
    });
    match (repeated, held.again) {
        (Some(repeated), Some(again)) => Some(repeated.min(again)),
        (repeated, again) => repeated.or(again),
    }
}

/// Where a settlement moves a job's row: its next time and the time it runs
/// for, the attempts counted, its error, and the run it records, none for a
/// job given back, which keeps the one before.
struct Moved {
    next: i64,
    at: i64,
    attempt: i64,
    error: Option<String>,
    ran: (Option<i64>, Option<i64>),
}

fn move_job(c: &Connection, queue: &QueueState, (next, id, key): Place<'_>, to: &Moved) -> Result<()> {
    let (ran, took) = to.ran;
    let moved = params![queue.id, next, id, to.next, to.at, to.attempt, to.error, ran, took];
    execute(c, MOVE_JOB, moved, "a settled job")?;
    match key {
        Some(key) => rows::follow(c, queue.id, key, next, to.next),
        None => Ok(()),
    }
}

/// The wait after attempt `attempt` failed: the initial span, doubling, never
/// past the most, and a tenth longer or shorter by the job, so that the jobs
/// one outage failed do not return in the same second:
///
/// ```text
/// 1 s to 1 h: 1 s, 2 s, 4 s … 34 m 8 s, 1 h, 1 h … each ± 10 %
/// ```
pub(crate) fn backoff(policy: &Policy, attempt: i64, id: i64) -> i64 {
    let (initial, most) = (crate::clock::millis(policy.backoff.0), crate::clock::millis(policy.backoff.1));
    let mut wait = initial;
    for _ in 1..attempt {
        if wait >= most {
            break;
        }
        wait = wait.saturating_mul(2);
    }
    let wait = wait.min(most);
    let spread = wait / 5 + 1;
    let jitter = i64::try_from(mix(id, attempt) % u64::try_from(spread).unwrap_or(1)).unwrap_or(0) - wait / 10;
    (wait + jitter).max(1)
}

/// Moves a job that repeats to its next run: its steps gone, its attempts
/// from none, and the error its last run ended with, when it failed; `ran` is
/// the run it records, none for a run no handler began.
fn run_again(
    c: &Connection,
    queue: &QueueState,
    place: Place<'_>,
    (next, error): (i64, Option<String>),
    ran: (Option<i64>, Option<i64>),
) -> Result<Settled> {
    rows::drop_steps(c, place.1)?;
    let to = Moved { next, at: next, attempt: 0, error, ran };
    move_job(c, queue, place, &to)?;
    Ok(Settled { due: Some(next), ..Settled::default() })
}

/// A number that varies with the job and its attempt, for the jitter.
fn mix(id: i64, attempt: i64) -> u64 {
    fnv1a(id.to_le_bytes().into_iter().chain(attempt.to_le_bytes()))
}

/// A failure as a job keeps it: its first `MAX_ERROR` bytes, cut on a
/// character, and `…` where more followed.
fn bounded(cause: &str) -> String {
    if cause.len() <= MAX_ERROR {
        return cause.to_owned();
    }
    let mut end = MAX_ERROR;
    while !cause.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &cause[..end])
}

fn changed_one(c: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<()> {
    let changed = c
        .prepare_cached(sql)
        .and_then(|mut statement| statement.execute(params))
        .map_err(|error| sql_error("a settlement: the job's lease", error))?;
    if changed == 0 {
        return Err(lost());
    }
    Ok(())
}

fn lost() -> Error {
    Error::new(ErrorKind::Conflict, "the job's lease ended and another claim or a change took it")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // contract: JOB-07
    #[test]
    fn a_retry_waits_longer_each_time_within_a_tenth_either_way() {
        let policy = Policy { backoff: (Duration::from_secs(1), Duration::from_secs(60)), ..Policy::default() };
        let waits: Vec<i64> = (1..=8).map(|attempt| backoff(&policy, attempt, 42)).collect();
        for (attempt, wait) in (1..).zip(&waits) {
            let expected = (1000i64 << (attempt - 1)).min(60_000);
            assert!((expected * 9 / 10..=expected * 11 / 10 + 1).contains(wait), "attempt {attempt}: {wait}");
        }
        assert_ne!(backoff(&policy, 3, 42), backoff(&policy, 3, 43), "two jobs, two waits");
    }

    #[test]
    fn a_kept_error_is_bounded_on_a_character() {
        let long = "é".repeat(MAX_ERROR);
        let kept = bounded(&long);
        assert!(kept.len() <= MAX_ERROR + '…'.len_utf8());
        assert!(kept.ends_with('…'));
    }
}
