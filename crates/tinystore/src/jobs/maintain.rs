//! What the queues keep no longer: failed jobs past their queue's `keep`,
//! done ids past their `dedupe`, and the ids jobs left behind.

use rusqlite::params;

use super::engine::Jobs;
use super::rows;
use crate::clock::millis;
use crate::sqlite::sql_error;
use crate::{Result, Store};

/// Rows one maintenance transaction removes, and transactions a call.
const BATCH: usize = 10_000;
const BATCHES: usize = 10;

const EXPIRE_FAILED: &str = "delete from _tinystore_jobs_failed where (queue, id) in (
        select queue, id from _tinystore_jobs_failed where queue = ?1 and failed <= ?2
        order by failed limit cast(?3 as integer))
    returning spill";
const FORGET_DONE: &str = "delete from _tinystore_jobs_done where (queue, key) in (
        select queue, key from _tinystore_jobs_done where until <= ?1 order by until limit cast(?2 as integer))
    returning spill";
/// A slice of a queue's ids in their order after a place, and those of it
/// whose job has left the queue.
const KEYS_SLICE: &str = "select count(*), coalesce(max(key), '') from (
    select key from _tinystore_jobs_keys where queue = ?1 and key > ?2 order by key limit cast(?3 as integer))";
const DROP_KEYS_LEFT: &str = "delete from _tinystore_jobs_keys where queue = ?1 and key > ?2 and key <= ?3
    and not exists (select 1 from _tinystore_jobs j where j.queue = _tinystore_jobs_keys.queue
        and j.next = _tinystore_jobs_keys.next and j.id = _tinystore_jobs_keys.id)";

/// What one maintenance pass removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Maintenance {
    /// Failed jobs past their queue's `keep`.
    pub failed: usize,
    /// Done ids past their queue's `dedupe`.
    pub done: usize,
    /// Ids that jobs gone from the queue left behind.
    pub ids: usize,
}

/// Removes what the queues this process opened keep no longer, 10,000 rows a
/// transaction. The store runs it every minute; a store opened without
/// background work leaves it to the caller.
pub fn maintain(store: &Store) -> Result<Maintenance> {
    let jobs = Jobs::of(store)?;
    run(&jobs)
}

pub(crate) fn run(jobs: &Jobs) -> Result<Maintenance> {
    let _one_at_a_time = jobs.hold_maintenance();
    let now = jobs.now();
    let mut done = Maintenance::default();
    for queue in jobs.open_queues() {
        let (id, cutoff) = (queue.id, now - millis(queue.policy.keep));
        done.failed += batches(jobs, move |tx| expire(tx, EXPIRE_FAILED, params![id, cutoff, BATCH as i64]))?;
        done.ids += drop_keys_left(jobs, id, &queue.keys_after)?;
    }
    done.done += batches(jobs, move |tx| expire(tx, FORGET_DONE, params![now, BATCH as i64]))?;
    jobs.file().sweep();
    Ok(done)
}

/// Runs one batch a transaction until a batch removes fewer than a full one,
/// at most `BATCHES` times.
fn batches(
    jobs: &Jobs,
    batch: impl Fn(&crate::sqlite::Tx<'_>) -> Result<usize> + Clone + Send + 'static,
) -> Result<usize> {
    let mut total = 0;
    for _ in 0..BATCHES {
        let removed = jobs.file().write(0, batch.clone())?;
        total += removed;
        if removed < BATCH {
            break;
        }
    }
    Ok(total)
}

/// Deletes one batch of rows and the values they spilled.
fn expire(c: &rusqlite::Connection, sql: &str, params: impl rusqlite::Params) -> Result<usize> {
    let read = || -> rusqlite::Result<Vec<Option<i64>>> {
        let mut delete = c.prepare_cached(sql)?;
        let rows = delete.query_map(params, |row| row.get(0))?;
        rows.collect()
    };
    let spilled = read().map_err(|error| sql_error("maintenance", error))?;
    for spill in &spilled {
        rows::drop_spill(c, *spill)?;
    }
    Ok(spilled.len())
}

/// Removes the ids that jobs gone from a queue left behind. It walks the
/// queue's ids in order from where the last call ended, and starts again from
/// the first once it has reached the last.
fn drop_keys_left(jobs: &Jobs, queue: i64, after: &std::sync::Mutex<String>) -> Result<usize> {
    let mut dropped = 0;
    for _ in 0..BATCHES {
        let from = after.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let (examined, last, removed) = jobs.file().write(0, move |tx| {
            let (examined, last): (i64, String) = tx
                .prepare_cached(KEYS_SLICE)
                .and_then(|mut select| {
                    select.query_row(params![queue, from, BATCH as i64], |row| Ok((row.get(0)?, row.get(1)?)))
                })
                .map_err(|error| sql_error("maintenance: ids", error))?;
            if examined == 0 {
                return Ok((0, last, 0));
            }
            let removed = tx
                .prepare_cached(DROP_KEYS_LEFT)
                .and_then(|mut delete| delete.execute(params![queue, from, last]))
                .map_err(|error| sql_error("maintenance: ids left behind", error))?;
            Ok((examined, last, removed))
        })?;
        dropped += removed;
        let finished = usize::try_from(examined).unwrap_or(0) < BATCH;
        *after.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = if finished { String::new() } else { last };
        if finished {
            break;
        }
    }
    Ok(dropped)
}
