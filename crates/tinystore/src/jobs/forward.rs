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

use super::call::Asked;
use super::engine::Jobs;
use super::queue::{Change, Queue};
use super::state::Kind;
use crate::replication::{In, Out};
use crate::{Error, ErrorKind, Result, Store};

/// Opens the engine at the home, which makes its file for a copy waiting
/// for it.
pub(crate) fn make(store: &Store) -> Result<()> {
    super::engine::Jobs::of(store).map(drop)
}

/// jobs' file, where its home passes writes.
pub(crate) const FILE: &str = "jobs.db";

/// Passes an add, a set or an update of a job of the queue `name` to the
/// home of the copy jobs' file is, and says whether it changed a job.
pub(crate) fn pass_on_write<V: Serialize>(
    store: &Store,
    name: &str,
    change: Change,
    asked: &Asked,
    value: &V,
) -> Result<bool> {
    let json = serde_json::to_vec(value).map_err(|error| Error::invalid(format!("a job that is no JSON: {error}")))?;
    let mut out = head(
        name,
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
pub(crate) fn pass_on_cancel(store: &Store, name: &str, id: &str) -> Result<bool> {
    let mut out = head(name, 3);
    out.text(id);
    answered(store, out)
}

/// Asks the home to register the queue `name`, which a copy found missing.
pub(crate) fn pass_on_open(store: &Store, name: &str) -> Result<()> {
    answered(store, head(name, 4)).map(drop)
}

fn head(name: &str, call: u8) -> Out {
    let mut out = Out(vec![crate::replication::JOBS]);
    out.text(name);
    out.byte(call);
    out
}

fn answered(store: &Store, out: Out) -> Result<bool> {
    let answer = crate::replication::pass_on(store, FILE, out.0)?;
    In { bytes: &answer, at: 0 }.byte().map(|changed| changed == 1)
}

/// Runs at the home a write another node passed on, and gives its answer.
pub(crate) fn run(store: &Store, bytes: &[u8]) -> Result<Vec<u8>> {
    let mut at = In { bytes, at: 0 };
    let name = at.text()?;
    let call = at.byte()?;
    let jobs = Jobs::of(store)?;
    let state = jobs.queue_as_open(&name, Kind::Queue)?;
    let queue: Queue<serde_json::Value> = Queue::of_state(jobs, state);
    let changed = match call {
        0..=2 => {
            let asked = Asked::read(&mut at)?;
            let value: serde_json::Value = serde_json::from_slice(at.bytes()?).map_err(|_| corrupt("a job's JSON"))?;
            let change = [Change::Add, Change::Set, Change::Update][usize::from(call)];
            queue.write_asked(change, asked, &value)?
        }
        3 => queue.cancel(at.text()?)?,
        4 => true,
        _ => return Err(corrupt("a call")),
    };
    if !at.done() {
        return Err(corrupt("a call that holds more than its fields"));
    }
    Ok(vec![u8::from(changed)])
}

pub(crate) fn corrupt(what: &str) -> Error {
    Error::new(ErrorKind::Corrupt, format!("a jobs write passed on: {what}"))
}
