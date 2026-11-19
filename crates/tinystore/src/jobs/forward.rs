//! A write on a copy of jobs.db, passed to the file's home: the queue by its
//! name, the call and its job as JSON, run there on the queue as the home
//! holds it open, or as a queue opens by default.
//!
//! ```text
//! name         the queue's
//! u8 call      0 add, 1 set, 2 update, 3 cancel, 4 open, 5 schedule, 6 cancel by row, 8 run now
//! add, set and update: what was asked, the job's JSON, then its own options
//! cancel:      the id
//! by row:      the row of a job without an id, as a page's cursor writes it
//! run now:     nothing more: the schedule is the queue
//! ```

use serde::Serialize;

use std::sync::Arc;

use super::call::Asked;
use super::engine::Jobs;
use super::queue::{Change, Queue};
use super::read::Row;
use super::read::named::Which;
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
pub(crate) use super::engine::FILE;

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
    asked.write_own(&mut out);
    answered(store, out)
}

/// Passes a cancel of the job `which` names to the home, and says whether
/// there was one.
pub(crate) fn pass_on_cancel(store: &Store, queue: (&str, Option<&str>), which: &Which) -> Result<bool> {
    let (call, named) = match which {
        Which::Id(id) => (3, id.clone()),
        Which::Row(row) => (6, row.to_string()),
    };
    let mut out = head(queue, call);
    out.text(&named);
    answered(store, out)
}

/// Passes a schedule's repeat, which the code on a copy opened it with, to
/// the home, which keeps its job: a schedule runs where its file is written.
pub(crate) fn pass_on_schedule(store: &Store, name: &str, repeat: &RepeatAsked) -> Result<()> {
    let mut out = head((name, None), 5);
    repeat.write(&mut out);
    answered(store, out).map(drop)
}

/// Passes a schedule's run now to the home, whose workers run it.
pub(crate) fn pass_on_run_now(store: &Store, name: &str) -> Result<()> {
    answered(store, head((name, None), 8)).map(drop)
}

/// Asks the home to register the queue `name`, which a copy found missing.
pub(crate) fn pass_on_open(store: &Store, queue: (&str, Option<&str>)) -> Result<()> {
    answered(store, head(queue, 4)).map(drop)
}

/// A call's first fields: the queue's name and the database whose file
/// keeps it, jobs.db when none.
fn head((name, database): (&str, Option<&str>), call: u8) -> Out {
    let mut out = Out(vec![crate::replication::JOBS]);
    out.text(name).maybe_text(database).byte(call);
    out
}

/// Passes a call on toward the home of the file that keeps its queue, which
/// `head` wrote: a database's queue goes where that database is written.
fn answered(store: &Store, out: Out) -> Result<bool> {
    let mut head = In { bytes: &out.0, at: 1 };
    let (_, database) = (head.text()?, head.maybe_text()?);
    let file = database.map_or_else(|| FILE.to_owned(), |database| crate::replication::database_file(&database));
    let answer = crate::replication::pass_on(store, &file, out.0)?;
    In { bytes: &answer, at: 0 }.byte().map(|changed| changed == 1)
}

/// Runs at the home a write another node passed on, and gives its answer
/// and the file it wrote: jobs.db, or the database's that keeps the queue.
pub(crate) fn run(store: &Store, lenders: crate::replication::Lenders<'_>, bytes: &[u8]) -> Result<(Vec<u8>, String)> {
    let mut at = In { bytes, at: 0 };
    let (name, database, call) = (at.text()?, at.maybe_text()?, at.byte()?);
    let (jobs, file) = match database {
        Some(database) => (Jobs::at(&lenders(&database)?)?, crate::replication::database_file(&database)),
        None => (Jobs::of(store)?, FILE.to_owned()),
    };
    let kind = if matches!(call, 5 | 8) { Kind::Schedule } else { Kind::Queue };
    let state = jobs.queue_as_open(&name, kind)?;
    let queue: Queue<serde_json::Value> = Queue::of_state(Arc::clone(&jobs), state);
    let changed = match call {
        0..=2 => {
            let mut asked = Asked::read(&mut at)?;
            let value: serde_json::Value = serde_json::from_slice(at.bytes()?).map_err(|_| corrupt("a job's JSON"))?;
            asked.read_own(&mut at)?;
            let change = [Change::Add, Change::Set, Change::Update][usize::from(call)];
            queue.write_asked(change, asked, &value)?
        }
        3 => queue.cancel(at.text()?)?,
        6 => {
            let row = Row::parse(&at.text()?).map_err(|_| corrupt("a job's row"))?;
            queue.cancel_in(&Which::Row(row), None)?
        }
        4 => true,
        5 => {
            let repeat = RepeatAsked::read(at.byte()?, &mut at)?;
            super::schedule::keep_repeat(&queue, &repeat.of(&name)?)?;
            true
        }
        8 => {
            super::schedule::run_now(&queue)?;
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
