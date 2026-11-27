//! What add, set, update, upsert, gather and cancel do to the job under an
//! id, by where that job is:
//!
//! | the id's job is      | add     | set                         | update                | upsert                      | gather                               | cancel                    |
//! |----------------------|---------|-----------------------------|-----------------------|-----------------------------|--------------------------------------|---------------------------|
//! | none                 | adds it | adds it                     | false                 | adds it                     | adds it                              | false                     |
//! | scheduled or waiting | false   | makes it new                | changes what it names | changes what differs        | appends, due when it settles         | removes it                |
//! | running              | false   | one run more after this one | false                 | next run takes what differs | its next run takes what comes        | tells its handler to stop |
//! | failed               | adds it | starts it again             | false                 | adds it                     | adds it                              | removes it                |
//! | done, kept by dedupe | false   | adds it again               | false                 | false                       | nothing                              | false                     |
//!
//! A done job holds its id while its own dedupe or its queue's lasts, and a
//! failed one is kept for its own keep or its queue's; a write that names
//! neither leaves a job's own as it was.

use rusqlite::Connection;

use super::gather::{self, Gathering, Wrote};
use super::read::Row;
use super::rows::{self, Change, Failed, NewJob, Own};
use super::state::QueueState;
use super::values::Kept;
use crate::Result;

/// A call's facts, gathered and checked before it waits for the writer.
#[derive(Debug)]
pub(crate) struct Prepared {
    pub(crate) key: Option<String>,
    /// When the job is due: the call's time, its repeat's next, or now.
    pub(crate) at: i64,
    /// Whether the call named a time.
    pub(crate) timed: bool,
    pub(crate) repeat: Option<String>,
    /// When the call's repeat runs next after now.
    pub(crate) repeated: Option<i64>,
    pub(crate) group: Option<String>,
    /// How long the job holds its id once done, as it keeps it.
    pub(crate) dedupe: Option<String>,
    /// How long it is kept once it failed for good, in milliseconds.
    pub(crate) keep: Option<i64>,
    /// When it leaves its queue unless written again, unix milliseconds.
    pub(crate) expires: Option<i64>,
    /// The ttl that term came from, which an upsert renews in steps of a
    /// thirtieth of it.
    pub(crate) ttl: Option<i64>,
    pub(crate) value: Kept,
    /// A gather's times and items.
    pub(crate) gather: Option<Gathering>,
    pub(crate) now: i64,
}

impl Prepared {
    /// The job the call adds, in the place of the failed one it took when
    /// there was one: that one's group when the call names none, and its error.
    fn new_job<'a>(&'a self, id: i64, queue: &QueueState, failed: Option<&'a Failed>) -> NewJob<'a> {
        NewJob {
            queue: queue.id,
            id,
            key: self.key.as_deref(),
            at: self.at,
            repeat: self.repeat.as_deref(),
            group: self.group.as_deref().or(failed.and_then(|failed| failed.group.as_deref())),
            error: failed.and_then(|failed| failed.error.as_deref()),
            own: self.own(),
            latest: self.gather.and_then(|gather| gather.latest),
            value: &self.value,
        }
    }

    fn change(&self, next: i64) -> Change<'_> {
        let (repeat, group) = (self.repeat.as_deref(), self.group.as_deref());
        Change { next, repeat, group, own: self.own(), value: &self.value }
    }

    fn own(&self) -> Own<'_> {
        Own { dedupe: self.dedupe.as_deref(), keep: self.keep, expires: self.expires }
    }
}

/// The job under the call's id, a live lease's or one before its term: a
/// job past it that no run holds is gone, and its id free, and the write
/// that finds it deletes it, as maintenance would.
fn there(c: &Connection, queue: &QueueState, call: &Prepared) -> Result<Option<rows::Row>> {
    let key = call.key.as_deref().unwrap_or_default();
    let Some(there) = rows::job_by_key(c, queue.id, key)? else {
        return Ok(None);
    };
    if there.expires.is_none_or(|expires| expires > call.now) || rows::lease_held(c, there.id, call.now)? {
        return Ok(Some(there));
    }
    rows::end_term(c, queue.id, &there)?;
    Ok(None)
}

/// Adds a job, to an id that is free; says whether it did. A failed job is
/// kept to be looked at and holds no id: the new job takes its place, and
/// its error until the new one's first run ends, so that a `get` still says
/// why the last one failed.
pub(crate) fn add(c: &Connection, queue: &QueueState, call: &Prepared) -> Result<bool> {
    if call.key.is_some() && there(c, queue, call)?.is_some() {
        return Ok(false);
    }
    add_free(c, queue, call)
}

/// Adds a job where no job waits or runs under the call's id: a done one its
/// dedupe keeps holds it still.
fn add_free(c: &Connection, queue: &QueueState, call: &Prepared) -> Result<bool> {
    let mut failed = None;
    if let Some(key) = &call.key {
        if rows::done_key(c, queue.id, key, call.now)? {
            return Ok(false);
        }
        failed = rows::drop_failed(c, queue.id, key)?;
    }
    rows::check_room(c, queue.id, queue.policy.max_waiting)?;
    rows::insert(c, &call.new_job(rows::next_id(c)?, queue, failed.as_ref()))?;
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
    if let Some(there) = there(c, queue, call)? {
        if rows::lease_held(c, there.id, call.now)? {
            let again = again(call, &there);
            rows::ask_again(c, queue.id, &there, again, &call.change(there.next))?;
            return Ok(Set { due: again.unwrap_or(call.at) });
        }
        rows::renew(c, queue.id, key, &there, &call.change(call.at))?;
        return Ok(Set { due: call.at });
    }
    // a set starts a failed job anew, its error gone with the rest of it
    let failed = rows::drop_failed(c, queue.id, key)?.map(|failed| Failed { error: None, ..failed });
    rows::check_room(c, queue.id, queue.policy.max_waiting)?;
    rows::insert(c, &call.new_job(rows::next_id(c)?, queue, failed.as_ref()))?;
    Ok(Set { due: call.at })
}

/// When the run a set asks of a running job is: its time when it named one;
/// the repeat's next time when the job repeats; now otherwise, as soon as the
/// run under way ends, since that run may have read what changed.
fn again(call: &Prepared, there: &rows::Row) -> Option<i64> {
    match (call.timed, call.repeat.is_some() || there.repeat.is_some()) {
        (true, _) => Some(call.at),
        (false, true) => None,
        (false, false) => Some(call.now),
    }
}

/// Adds the job when its id is free, as `add` does, or brings the one there
/// to the call: its value, group, repeat and options of its own where they
/// differ, its time, attempts and steps as they were. A changed value drops
/// the steps kept for the old one; a changed repeat moves a job not yet due
/// to the repeat's next time, and one due stays due. A running job's run
/// goes on, and its next run, when it has one, takes what differs. Answers
/// when the job is due when the call changed it, none when the job already
/// was what the call says, which writes nothing.
pub(crate) fn upsert(c: &Connection, queue: &QueueState, call: &Prepared) -> Result<Option<i64>> {
    let key = call.key.as_deref().unwrap_or_default();
    let Some(there) = there(c, queue, call)? else {
        return Ok(add(c, queue, call)?.then_some(call.at));
    };
    let shape = rows::shape(c, queue.id, &there, &call.value)?;
    let differs = |asked: Option<&str>, kept: Option<&str>| asked.is_some() && asked != kept;
    let repeat_changed = differs(call.repeat.as_deref(), there.repeat.as_deref());
    let changed = !shape.same_value
        || repeat_changed
        || differs(call.group.as_deref(), shape.group.as_deref())
        || differs(call.dedupe.as_deref(), shape.dedupe.as_deref())
        || call.keep.is_some() && call.keep != shape.keep
        || call.ttl.is_none() && call.expires.is_some() && call.expires != shape.expires;
    if !changed {
        // a renewal alone is written once a thirtieth of its ttl, as an idle key's is, and changes nothing
        if let (Some(ttl), Some(expires)) = (call.ttl, call.expires)
            && shape.expires.is_none_or(|kept| expires - kept >= ttl / 30)
        {
            rows::renew_term(c, queue.id, &there, expires)?;
        }
        return Ok(None);
    }
    if rows::lease_held(c, there.id, call.now)? {
        // a job that ends with its run has no next run to take the change
        if there.repeat.is_none() && call.repeat.is_none() && there.again.is_none() {
            return Ok(None);
        }
        rows::ask_again(c, queue.id, &there, there.again, &call.change(there.next))?;
        return Ok(Some(rows::due_of(there.next)));
    }
    let next = match call.repeated {
        Some(repeated) if repeat_changed && rows::due_of(there.next) > call.now => repeated,
        _ => there.next,
    };
    rows::reshape(c, (queue.id, key), &there, &call.change(next), !shape.same_value)?;
    Ok(Some(rows::due_of(next)))
}

/// Gathers a call's items into the id's job, as [`JobCall::gather`] says:
/// one read of the job and its value, then its write. A running job's run
/// read its list at its claim, so a gather appends after it, asks a run more
/// with `again`, and the first since the claim keeps in `taken` how many of
/// the list's items the run read: its settlement drops them once the run
/// ends well, and keeps them for a retry. Its count is what the next run
/// takes.
///
/// [`JobCall::gather`]: super::JobCall::gather
pub(crate) fn gather(c: &Connection, queue: &QueueState, call: &Prepared) -> Result<Wrote> {
    let (key, asked) = (call.key.as_deref().unwrap_or_default(), call.gather.as_ref());
    let asked = asked.ok_or_else(|| crate::Error::internal("a gather's write without its gathering"))?;
    let found = match rows::gathering(c, queue.id, key, call.now)? {
        Some((there, kept)) if there.expires.is_some_and(|expires| expires <= call.now) && !kept.held => {
            rows::end_term(c, queue.id, &there)?;
            None
        }
        found => found,
    };
    let Some((there, kept)) = found else {
        let added = add_free(c, queue, call)?;
        let (count, due) = if added { (asked.count, call.at) } else { (0, call.now) };
        return Ok(Wrote { added, count, due });
    };
    if there.repeat.is_some() {
        return Err(crate::Error::invalid("a job that repeats gathers nothing: its list would run each time"));
    }
    let (joined, before, count) = gather::appended(&kept.value, call.value.bytes())?;
    // the first gather since the claim counts what the run read: the whole
    // list, unless a set since gave the job a value the run never saw
    let first_since_claim = kept.held && kept.taken.is_none();
    let taken = match (kept.held, kept.taken) {
        (false, _) => None,
        (true, Some(taken)) => Some(taken),
        (true, None) if there.again.is_none() => Some(i64::try_from(before).unwrap_or(i64::MAX)),
        (true, None) => Some(0),
    };
    let latest = if first_since_claim { asked.latest } else { kept.latest.or(asked.latest) };
    let settles = latest.map_or(asked.settled, |latest| latest.min(asked.settled));
    // a job waiting for a retry keeps its backoff: it runs at the later of the two
    let waits_for_retry = !kept.held && kept.attempt > 0;
    let settles = if waits_for_retry { settles.max(rows::due_of(there.next)) } else { settles };
    let due = call.gathers_held(kept.quiet.as_deref(), settles)?;
    let value = Kept::of(joined);
    let change = Change { next: due, repeat: None, group: call.group.as_deref(), own: call.own(), value: &value };
    rows::gather(c, (queue.id, key), &there, (latest, taken, kept.held), &change)?;
    let count = count - taken.map_or(0, |taken| u64::try_from(taken).unwrap_or(0));
    Ok(Wrote { added: first_since_claim, count, due })
}

/// Changes a job that is not running, one waiting for a retry among them: its
/// value, and its time, group or repeat when the call names them. Answers when
/// it is due, none when there is no such job or it runs.
pub(crate) fn update(c: &Connection, queue: &QueueState, call: &Prepared) -> Result<Option<i64>> {
    let key = call.key.as_deref().unwrap_or_default();
    let Some(there) = there(c, queue, call)? else {
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
        return took(c, queue, &taken);
    }
    Ok(if rows::drop_failed(c, queue.id, key)?.is_some() { Cancelled::Failed } else { Cancelled::Nothing })
}

/// Takes the job without an id that lies in `row`, as `cancel` takes one by
/// its id; a job no longer there is nothing, never another (JOB-15).
pub(crate) fn cancel_row(c: &Connection, queue: &QueueState, row: Row) -> Result<Cancelled> {
    match row {
        Row::Queued { next, number } => match rows::cancel_row(c, queue.id, rows::due_of(next), number)? {
            Some(taken) => took(c, queue, &taken),
            None => Ok(Cancelled::Nothing),
        },
        Row::Failed { failed, number } => {
            Ok(if rows::drop_failed_row(c, queue.id, failed, number)? { Cancelled::Failed } else { Cancelled::Nothing })
        }
    }
}

/// A job a cancel took from the queue's rows: one of a group that held or
/// was due to take its group's place gives it to the group's first parked job.
fn took(c: &Connection, queue: &QueueState, taken: &rows::Taken) -> Result<Cancelled> {
    if let (Some(group), true, Some(_)) = (&taken.group, taken.placed, queue.policy.concurrency.group) {
        rows::unpark_one(c, queue.id, group)?;
    }
    Ok(Cancelled::Job(taken.id))
}
