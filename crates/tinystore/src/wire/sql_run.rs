//! A mirror's replace in parts over the protocol: a run begun, its parts
//! added, and its commit, whose difference goes in parts within the
//! client's credit. A run is its connection's: the connection's end lets go
//! of every run it began, and each writes nothing.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use super::{Handles, Link, Queried, part_bound, replace_mode, unit_named, value_of};
use crate::limits::{ANSWER_BYTES, Limit};
use crate::sql::{Fits, Found, Held, Listed, Shape, Staged, Value, row_bytes};
use crate::sync::lock;
use crate::wire::Answered;
use crate::wire::codec::{Cell, Message};
use crate::wire::protocol::{Empty, Failure, Handle, SqlAdd, SqlBegin, SqlDiff, SqlRows, SqlRun};

/// The runs one connection holds at once: past them a begin is `limit`.
const AT_ONCE: Limit = Limit { name: "mirror runs", bound: 64 };

/// A connection's runs by the handle each begin answered.
#[derive(Default)]
pub(crate) struct Runs {
    next: u64,
    open: HashMap<u64, Arc<Staged>>,
}

impl Runs {
    /// Keeps a run begun, once the runs that waited past their bound let go
    /// of what they held.
    fn keep(&mut self, run: Staged) -> Result<u64, Failure> {
        self.open.retain(|_, run| !run.idle());
        if self.open.len() as u64 >= AT_ONCE.bound {
            let why = format!("{} runs at once, past {}: commit or drop one first", self.open.len() + 1, AT_ONCE.bound);
            return Err(Failure::from(AT_ONCE.refuse(self.open.len() as u64 + 1, why)));
        }
        self.next += 1;
        self.open.insert(self.next, Arc::new(run));
        Ok(self.next)
    }

    fn get(&self, run: u64) -> Result<Arc<Staged>, Failure> {
        self.open.get(&run).cloned().ok_or_else(|| unknown(run))
    }

    fn take(&mut self, run: u64) -> Result<Arc<Staged>, Failure> {
        self.open.remove(&run).ok_or_else(|| unknown(run))
    }

    pub(crate) fn clear(&mut self) {
        self.open.clear();
    }
}

fn unknown(run: u64) -> Failure {
    Failure::invalid(format!("run {run} names no run this connection began: committed, dropped or never begun"))
}

/// Begins a run: its table checked on a reader, as a mirror's call checks it.
pub(crate) fn begin(handles: &Handles, asked: SqlBegin) -> Answered {
    let database = handles.database(asked.handle)?;
    let describe = format!("sql {}: mirror {}", database.name(), asked.table);
    let unit = unit_named(asked.gone_unit.as_deref())
        .map_err(|failure| Failure::invalid(format!("{describe}: gone {}", failure.message)))?;
    let shape = Shape { table: asked.table, key: asked.key, gone: asked.gone, unit, change_on: asked.change_on };
    let mode = replace_mode(&describe, (asked.scope, asked.scope_values), asked.min_kept)?;
    let run = Staged::begin(&database, Arc::new(shape), mode, describe)?;
    let run = lock(&handles.runs).keep(run)?;
    Ok(Handle { handle: run }.encode())
}

/// Lets go of a run, which writes nothing.
pub(crate) fn drop(handles: &Handles, asked: SqlRun) -> Answered {
    lock(&handles.runs).take(asked.run)?;
    Ok(Empty {}.encode())
}

/// Adds a part to a run; `done` answers once it is kept.
pub(crate) fn add(handles: &Handles, body: &[u8], done: impl FnOnce(Answered) + Send + 'static) {
    let asked =
        SqlAdd::decode(body).and_then(|asked| Ok((lock(&handles.runs).get(asked.run)?, listed_of(asked.rows)?)));
    match asked {
        Ok((run, listed)) => run.add_then(listed, move |kept| {
            done(kept.map(|()| Empty {}.encode()).map_err(Failure::from));
        }),
        Err(failure) => done(Err(failure)),
    }
}

/// Commits a run: its difference whole when one part holds it, in parts
/// otherwise, each row refused before anything is written when one alone
/// passes a part.
pub(crate) fn commit(
    handles: &Handles,
    body: &[u8],
    link: &Link,
    done: impl FnOnce(Result<Queried, Failure>) + Send + 'static,
) {
    let run = match SqlRun::decode(body).and_then(|asked| lock(&handles.runs).take(asked.run)) {
        Ok(run) => run,
        Err(failure) => return done(Err(failure)),
    };
    let bound = part_bound(link);
    let answer = Fits::Parts(Limit { bound: bound as u64, ..ANSWER_BYTES });
    run.commit_then(Some(answer), move |found| {
        done(found.map_err(Failure::from).map(|found| diff_parts(found, bound)));
    });
}

fn listed_of(rows: SqlRows) -> Result<Listed, Failure> {
    let cells = rows.rows.into_iter().map(|row| row.into_iter().map(value_of).collect::<Result<Vec<_>, _>>());
    Ok(Listed { columns: rows.columns, rows: cells.collect::<Result<_, _>>()? })
}

/// Which of a difference's lists a row goes in.
type Target = fn(&mut SqlDiff) -> &mut Vec<Vec<Cell>>;

/// A difference cut into parts of `bound` bytes at most, each with the
/// columns, the last with `same` and `suspect`: the client joins each list
/// of every part, in their order, so that `before` and `after` stay paired.
fn diff_parts(found: Found, bound: usize) -> Queried {
    let mut parts = Parting { columns: found.columns.clone(), bound, parts: VecDeque::new(), part: None, bytes: 0 };
    let added: Target = |diff| &mut diff.added;
    let (before, after): (Target, Target) = (|diff| &mut diff.before, |diff| &mut diff.after);
    let gone: Target = |diff| &mut diff.gone;
    found.added.into_iter().for_each(|row| parts.push(row, added));
    for (was, is) in found.changed {
        parts.push(was, before);
        parts.push(is, after);
    }
    found.gone.into_iter().for_each(|row| parts.push(row, gone));
    parts.end(found.same, found.suspect)
}

struct Parting {
    columns: Vec<String>,
    bound: usize,
    parts: VecDeque<Vec<u8>>,
    part: Option<SqlDiff>,
    bytes: usize,
}

impl Parting {
    fn push(&mut self, row: Vec<Value>, into: Target) {
        let weight = row_bytes(&row);
        if self.part.is_some() && self.bytes + weight > self.bound {
            self.seal();
        }
        let columns = &self.columns;
        let part = self.part.get_or_insert_with(|| SqlDiff { columns: columns.clone(), ..SqlDiff::default() });
        if self.bytes == 0 {
            self.bytes = columns.iter().map(|column| column.len() + 5).sum::<usize>() + 32;
        }
        into(part).push(row.into_iter().map(super::cell_of).collect());
        self.bytes += weight;
    }

    fn seal(&mut self) {
        if let Some(part) = self.part.take() {
            self.parts.push_back(part.encode());
        }
        self.bytes = 0;
    }

    fn end(mut self, same: u64, suspect: bool) -> Queried {
        let columns = self.columns.clone();
        let part = self.part.get_or_insert_with(|| SqlDiff { columns, ..SqlDiff::default() });
        (part.same, part.suspect) = (same, suspect);
        self.seal();
        if self.parts.len() == 1 {
            return Queried::Whole(self.parts.pop_front().unwrap_or_default());
        }
        Queried::Parts(self.parts, Held::default())
    }
}
