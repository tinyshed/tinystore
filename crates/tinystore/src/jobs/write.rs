//! What add, set, update and cancel do to the job under an id, by where that
//! job is:
//!
//! | the id's job is      | add            | set                         | update                    | cancel                     |
//! |----------------------|----------------|-----------------------------|---------------------------|----------------------------|
//! | none                 | adds it        | adds it                     | false                     | false                      |
//! | scheduled or waiting | false          | makes it new                | changes what it names     | removes it                 |
//! | running              | false          | one run more after this one | false                     | tells its handler to stop  |
//! | failed               | false          | starts it again             | false                     | removes it                 |
//! | done, kept by dedupe | false          | adds it again               | false                     | false                      |

use rusqlite::Connection;

use super::rows::{self, Change, NewJob, Row};
use super::state::QueueState;
use super::values::Kept;
use crate::Result;

/// A call's facts, gathered and checked before it waits for the writer.
#[derive(Debug)]
pub(crate) struct Prepared {
    /// The id a new row takes.
    pub(crate) id: i64,
    pub(crate) key: Option<String>,
    /// When the job is due: the call's time, its repeat's next, or now.
    pub(crate) at: i64,
    /// Whether the call named a time.
    pub(crate) timed: bool,
    pub(crate) repeat: Option<String>,
    pub(crate) group: Option<String>,
    pub(crate) value: Kept,
    pub(crate) now: i64,
}

impl Prepared {
    fn new_job<'a>(&'a self, queue: &QueueState, group: Option<&'a str>) -> NewJob<'a> {
        NewJob {
            queue: queue.id,
            id: self.id,
            key: self.key.as_deref(),
            at: self.at,
            repeat: self.repeat.as_deref(),
            group,
            value: &self.value,
        }
    }

    fn change(&self, next: i64) -> Change<'_> {
        Change { next, repeat: self.repeat.as_deref(), group: self.group.as_deref(), value: &self.value }
    }
}

/// Adds a job, to an id that is free; says whether it did.
pub(crate) fn add(c: &Connection, queue: &QueueState, call: &Prepared) -> Result<bool> {
    if let Some(key) = &call.key {
        let taken = rows::job_by_key(c, queue.id, key)?.is_some()
            || rows::failed_key(c, queue.id, key)?
            || queue.policy.dedupe.is_some() && rows::done_key(c, queue.id, key, call.now)?;
        if taken {
            return Ok(false);
        }
    }
    rows::check_room(c, queue.id, queue.policy.max_waiting)?;
    rows::insert(c, &call.new_job(queue, call.group.as_deref()))?;
    Ok(true)
}

/// What a set did: when the job runs next.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Set {
    pub(crate) due: i64,
}

/// Makes the id's job this value at this time, whatever it was: a set needs
/// an id. A group or a repeat it does not name stays as it was.
pub(crate) fn set(c: &Connection, queue: &QueueState, call: &Prepared) -> Result<Set> {
    let key = call.key.as_deref().unwrap_or_default();
    if let Some(there) = rows::job_by_key(c, queue.id, key)? {
        if rows::lease_held(c, there.id, call.now)? {
            let again = again(call, &there);
            rows::ask_again(c, queue.id, &there, again, &call.change(there.next))?;
            return Ok(Set { due: again.unwrap_or(call.at) });
        }
        rows::renew(c, queue.id, key, &there, &call.change(call.at))?;
        return Ok(Set { due: call.at });
    }
    let failed_group = rows::drop_failed(c, queue.id, key)?.flatten();
    rows::check_room(c, queue.id, queue.policy.max_waiting)?;
    rows::insert(c, &call.new_job(queue, call.group.as_deref().or(failed_group.as_deref())))?;
    Ok(Set { due: call.at })
}

/// When the run a set asks of a running job is: its time when it named one;
/// the repeat's next time when the job repeats; now otherwise, as soon as the
/// run under way ends, since that run may have read what changed.
fn again(call: &Prepared, there: &Row) -> Option<i64> {
    match (call.timed, call.repeat.is_some() || there.repeat.is_some()) {
        (true, _) => Some(call.at),
        (false, true) => None,
        (false, false) => Some(call.now),
    }
}

/// Changes a job that has not started: its value, and its time, group or
/// repeat when the call names them. Answers when it is due, none when there is
/// no such job or it runs.
pub(crate) fn update(c: &Connection, queue: &QueueState, call: &Prepared) -> Result<Option<i64>> {
    let key = call.key.as_deref().unwrap_or_default();
    let Some(there) = rows::job_by_key(c, queue.id, key)? else {
        return Ok(None);
    };
    if rows::lease_held(c, there.id, call.now)? {
        return Ok(None);
    }
    // a parked job goes back to its time, and a claim parks it again while
    // its group is full
    let next = if call.timed || call.repeat.is_some() { call.at } else { rows::due_of(there.next) };
    rows::update(c, queue.id, key, &there, &call.change(next))?;
    Ok(Some(next))
}

/// What a cancel took: a job of the queue's rows, by its number, or a failed
/// one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cancelled {
    Job(i64),
    Failed,
    Nothing,
}

/// Takes the job under an id, whatever its state. A job of a group that held
/// or was due to take its group's place gives it to the group's first parked
/// job.
pub(crate) fn cancel(c: &Connection, queue: &QueueState, key: &str) -> Result<Cancelled> {
    if let Some(taken) = rows::cancel(c, queue.id, key)? {
        if let (Some(group), true, Some(_)) = (&taken.group, taken.placed, queue.policy.concurrency.group) {
            rows::unpark_one(c, queue.id, group)?;
        }
        return Ok(Cancelled::Job(taken.id));
    }
    Ok(if rows::drop_failed(c, queue.id, key)?.is_some() { Cancelled::Failed } else { Cancelled::Nothing })
}
