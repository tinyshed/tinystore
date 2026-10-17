//! A write on a copy of jobs.db, passed to the file's home: the queue by its
//! name, the call and its job as JSON, run there on the queue as the home
//! holds it open, or as a queue opens by default.
//!
//! ```text
//! name         the queue's
//! u8 call      0 add, 1 set, 2 update, 3 cancel, 4 open
//! add, set and update: what was asked, then the job's JSON
//! cancel:      the id
//! ```

use serde::Serialize;

use std::sync::Arc;

use super::call::Asked;
use super::engine::Jobs;
use super::queue::{Change, Queue};
use super::repeat::Asked as RepeatAsked;
use super::state::Kind;
use crate::replication::{In, Out};
use crate::{Error, ErrorKind, Result, Store};

/// Opens the engine at the home, which makes its file for a copy waiting
/// for it.
pub(crate) fn make(store: &Store) -> Result<()> {
    super::engine::Jobs::of(store).map(drop)
}

/// Opens jobs in the file `home` lends, which makes its tables there.
pub(crate) fn make_in(home: &crate::engine::Home) -> Result<()> {
    Jobs::at(home).map(drop)
}

/// jobs' file, where its home passes writes.
pub(crate) const FILE: &str = "jobs.db";

/// Passes an add, a set or an update of a job of the queue `name` to the
/// home of the copy jobs' file is, and says whether it changed a job.
pub(crate) fn pass_on_write<V: Serialize>(
    store: &Store,
    (name, database): (&str, Option<&str>),
    change: Change,
    asked: &Asked,
    value: &V,
) -> Result<bool> {
    let json = serde_json::to_vec(value).map_err(|error| Error::invalid(format!("a job that is no JSON: {error}")))?;
    let mut out = head(
        (name, database),
        match change {
            Change::Add => 0,
            Change::Set => 1,
            Change::Update => 2,
        },
    );
    asked.write(&mut out);
    out.bytes(&json);
    answered(store, out)
}

/// Passes a cancel of the job `id` to the home, and says whether there was one.
pub(crate) fn pass_on_cancel(store: &Store, queue: (&str, Option<&str>), id: &str) -> Result<bool> {
    let mut out = head(queue, 3);
    out.text(id);
    answered(store, out)
}

/// Passes a schedule's repeat, which the code on a copy opened it with, to
/// the home, which keeps its job: a schedule runs where its file is written.
pub(crate) fn pass_on_schedule(store: &Store, name: &str, repeat: &RepeatAsked) -> Result<()> {
    let mut out = head((name, None), 5);
    match repeat {
        RepeatAsked::Every(span) => {
            out.byte(1);
            out.long(u64::try_from(span.as_millis()).unwrap_or(u64::MAX));
        }
        RepeatAsked::Cron { expr, zone } => {
            out.byte(2);
            out.text(expr);
            out.text(zone);
        }
    }
    answered(store, out).map(drop)
}

/// Asks the home to register the queue `name`, which a copy found missing.
pub(crate) fn pass_on_open(store: &Store, queue: (&str, Option<&str>)) -> Result<()> {
    answered(store, head(queue, 4)).map(drop)
}

/// A call's first fields: the queue's name and the database whose file
/// keeps it, jobs.db when none.
fn head((name, database): (&str, Option<&str>), call: u8) -> Out {
    let mut out = Out(vec![crate::replication::JOBS]);
    out.text(name);
    out.byte(u8::from(database.is_some()));
    out.text(database.unwrap_or(""));
    out.byte(call);
    out
}

fn answered(store: &Store, out: Out) -> Result<bool> {
    let answer = crate::replication::pass_on(store, FILE, out.0)?;
    In { bytes: &answer, at: 0 }.byte().map(|changed| changed == 1)
}

/// Runs at the home a write another node passed on, and gives its answer
/// and the file it wrote: jobs.db, or the database's that keeps the queue.
pub(crate) fn run(store: &Store, lenders: crate::replication::Lenders<'_>, bytes: &[u8]) -> Result<(Vec<u8>, String)> {
    let mut at = In { bytes, at: 0 };
    let name = at.text()?;
    let lent = at.byte()? == 1;
    let database = at.text()?;
    let call = at.byte()?;
    let (jobs, file) = if lent {
        (Jobs::at(&lenders(&database)?)?, crate::replication::database_file(&database))
    } else {
        (Jobs::of(store)?, FILE.to_owned())
    };
    let kind = if call == 5 { Kind::Schedule } else { Kind::Queue };
    let state = jobs.queue_as_open(&name, kind)?;
    let queue: Queue<serde_json::Value> = Queue::of_state(Arc::clone(&jobs), state);
    let changed = match call {
        0..=2 => {
            let asked = Asked::read(&mut at)?;
            let value: serde_json::Value = serde_json::from_slice(at.bytes()?).map_err(|_| corrupt("a job's JSON"))?;
            let change = [Change::Add, Change::Set, Change::Update][usize::from(call)];
            queue.write_asked(change, asked, &value)?
        }
        3 => queue.cancel(at.text()?)?,
        4 => true,
        5 => {
            let repeat = match at.byte()? {
                1 => RepeatAsked::Every(std::time::Duration::from_millis(at.long()?)),
                2 => RepeatAsked::Cron { expr: at.text()?, zone: at.text()? },
                _ => return Err(corrupt("a schedule's repeat")),
            };
            super::schedule::keep_repeat(&queue, &repeat.of(&name)?)?;
            true
        }
        _ => return Err(corrupt("a call")),
    };
    if !at.done() {
        return Err(corrupt("a call that holds more than its fields"));
    }
    Ok((vec![u8::from(changed)], file))
}

pub(crate) fn corrupt(what: &str) -> Error {
    Error::new(ErrorKind::Corrupt, format!("a jobs write passed on: {what}"))
}
