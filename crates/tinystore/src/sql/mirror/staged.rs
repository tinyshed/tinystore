//! A mirror's replace in parts: a list too large for one call is given a
//! part at a time and compared with the table once, at the commit, so that
//! what no part named is gone only then. A run never committed writes
//! nothing to the file.
//!
//! The parts wait in a temporary table on the file's writer, one a run:
//!
//! - not in memory, which would hold up to 100,000 rows for as long as the
//!   program takes between two parts;
//! - not in a table of the file, which would go into its WAL, its backups and
//!   its nodes' copies, and stay behind a process that died.
//!
//! A temporary table is the writer connection's alone: SQLite keeps it in a
//! temporary file of its own, past its page cache, and deletes that file when
//! the connection closes, the store's close or a process that died. No
//! reader, backup or copy sees it. A temporary table is found before the
//! file's own by every statement on the connection, so its name starts with
//! `__tinystore`, which a program's table never does, and its columns are
//! `c0`, `c1`…, the part's names kept here.
//!
//! A given statement may make no temporary table (`sqlite/given.rs`), so a
//! run's statements are the store's own. They are prepared outside the cache
//! of statements: a statement kept there is found again by its text, and a
//! given statement of the same text would run it unseen by the authorizer.
//!
//! A run lives while calls reach it: one no call reached for five minutes,
//! by the store's clock, ends, and its next call is `limit`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use rusqlite::Connection;

use super::list::{Fits, Found, Given, Listed, Mode, Running, Shape, mirror_on};
use super::{refuse_on_a_copy, table};
use crate::limits::{Limit, MIRROR_ROWS};
use crate::sql::database::Database;
use crate::sql::rows::Held;
use crate::sql::values::Value;
use crate::sqlite::Tx;
use crate::sync::lock;
use crate::{Error, Result};

/// How long a run waits for its next call.
pub(crate) const IDLE: Duration = Duration::from_secs(5 * 60);
/// [`IDLE`] as a refusal names it, in milliseconds.
const IDLE_HELD: Limit = Limit { name: "mirror run idle millis", bound: IDLE.as_millis() as u64 };

/// Numbers every run's table in the process, so that two never meet.
static RUNS: AtomicU64 = AtomicU64::new(1);

/// A replace whose list comes in parts; [`Staged::begin`] opens one.
pub(crate) struct Staged {
    db: Database,
    shape: Arc<Shape>,
    mode: Mode,
    describe: String,
    state: Arc<Mutex<State>>,
}

/// What a run has taken, shared with the writes that take its parts.
struct State {
    /// `temp."__tinystore_run_7"`, quoted.
    table: String,
    /// The first part's columns, which every part gives; none until a part
    /// with rows made the table.
    columns: Option<Vec<String>>,
    rows: u64,
    touched: SystemTime,
    ended: Option<Ended>,
}

/// Why a run takes no more calls.
enum Ended {
    /// Its commit began: the parts sent before it are still taken, in their
    /// order, ahead of it.
    Committed,
    /// It waited past [`IDLE`] for a call.
    Idle,
    /// A part failed, or the run was let go of: why.
    Refused(String),
}

impl Ended {
    fn refusal(&self) -> Error {
        match self {
            Ended::Committed => Error::invalid("the run is committed: begin another"),
            Ended::Idle => IDLE_HELD.run_out(format!("the run waited past its {IDLE:?} for a call, and ended")),
            Ended::Refused(why) => Error::invalid(why.clone()),
        }
    }
}

impl Staged {
    /// A run of `mode`, a replace, on the mirror `shape` names: its table is
    /// checked on a reader now, as an open checks it.
    pub(crate) fn begin(db: &Database, shape: Arc<Shape>, mode: Mode, describe: String) -> Result<Staged> {
        refuse_on_a_copy(db, &describe)?;
        db.file().read(|connection| table::check(connection, &shape)).map_err(|error| error.within(&describe))?;
        let table = format!("temp.\"__tinystore_run_{}\"", RUNS.fetch_add(1, Ordering::Relaxed));
        let state = State { table, columns: None, rows: 0, touched: db.now(), ended: None };
        Ok(Staged { db: db.clone(), shape, mode, describe, state: Arc::new(Mutex::new(state)) })
    }

    /// Takes a part, checked as a list of one call is, and waits for it.
    pub(crate) fn add(&self, listed: Listed) -> Result<()> {
        let (weight, write) = self.failed_on(self.part(listed)).map_err(|error| error.within(&self.describe))?;
        let written = self.db.file().write(weight, write);
        self.failed_on(written).map_err(|error| error.within(&self.describe))
    }

    /// [`Staged::add`] without waiting: `done` is called on the thread that
    /// commits it.
    #[cfg(any(feature = "wire", feature = "async"))]
    pub(crate) fn add_then(&self, listed: Listed, done: impl FnOnce(Result<()>) + Send + 'static) {
        let describe = self.describe.clone();
        let (weight, write) = match self.failed_on(self.part(listed)) {
            Ok(part) => part,
            Err(error) => return done(Err(error.within(&describe))),
        };
        let state = Arc::clone(&self.state);
        self.db.file().submit(weight, write, move |written| {
            done(failed(&state, written).map_err(|error| error.within(&describe)));
        });
    }

    /// Compares every part with what the scope holds and writes the
    /// difference in one write, the run's table dropped in it; the run takes
    /// no call after.
    pub(crate) fn commit(&self) -> Result<Found> {
        let write = self.committing().map_err(|error| error.within(&self.describe))?;
        let found = self.db.file().write(0, write);
        self.dropped_on(found).map_err(|error| error.within(&self.describe))
    }

    /// [`Staged::commit`] without waiting, its answer held within `answer`
    /// as the wire's [`Running::answer`] says.
    #[cfg(any(feature = "wire", feature = "async"))]
    pub(crate) fn commit_then(&self, answer: Option<Fits>, done: impl FnOnce(Result<Found>) + Send + 'static) {
        let describe = self.describe.clone();
        let write = match self.committing_within(answer) {
            Ok(write) => write,
            Err(error) => return done(Err(error.within(&describe))),
        };
        let (db, state) = (self.db.clone(), Arc::clone(&self.state));
        self.db.file().submit(0, write, move |found| {
            if found.is_err() {
                drop_table(&db, &state);
            }
            done(found.map_err(|error| error.within(&describe)));
        });
    }

    /// The run as an error names it: `sql lms: mirror works`.
    pub(crate) fn describe(&self) -> &str {
        &self.describe
    }

    /// Whether the run waited past [`IDLE`] since its last call, by the
    /// store's clock: one that did ends.
    #[cfg(feature = "wire")]
    pub(crate) fn idle(&self) -> bool {
        let state = lock(&self.state);
        state.ended.is_none() && waited(&state, self.db.now())
    }

    /// The part checked and the write that keeps it in the run's table.
    fn part(&self, listed: Listed) -> Result<(usize, impl FnOnce(&Tx<'_>) -> Result<()> + Send + 'static)> {
        self.touch()?;
        let given = Given::checked(&self.shape, listed, &self.mode)?;
        let (weight, listed) = (given.weight(), given.into_listed());
        let state = Arc::clone(&self.state);
        Ok((weight, move |tx: &Tx<'_>| keep(tx, &state, listed)))
    }

    fn committing(&self) -> Result<impl FnOnce(&Tx<'_>) -> Result<Found> + Send + 'static> {
        self.committing_within(None)
    }

    /// The commit's write: the parts read back, checked whole as one list
    /// is, compared and written, and the run's table dropped.
    fn committing_within(
        &self,
        answer: Option<Fits>,
    ) -> Result<impl FnOnce(&Tx<'_>) -> Result<Found> + Send + 'static> {
        self.touch()?;
        lock(&self.state).ended = Some(Ended::Committed);
        let (shape, mode, state) = (Arc::clone(&self.shape), self.mode.clone(), Arc::clone(&self.state));
        let (memory, now) = (Arc::clone(self.db.memory()), shape.unit.of(self.db.now()));
        Ok(move |tx: &Tx<'_>| {
            let (listed, _held) = read_back(tx, &state, &memory)?;
            let given = Given::checked(&shape, listed, &mode)?;
            let found = mirror_on(tx, &shape, (&given, &mode), &Running { now, memory: &memory, answer })?;
            drop_on(tx, &state)?;
            Ok(found)
        })
    }

    /// Refuses a call on a run that ended, or that waited past [`IDLE`],
    /// which ends it; marks the run reached now otherwise.
    fn touch(&self) -> Result<()> {
        let now = self.db.now();
        let mut state = lock(&self.state);
        if let Some(ended) = &state.ended {
            return Err(ended.refusal());
        }
        if waited(&state, now) {
            state.ended = Some(Ended::Idle);
            return Err(Ended::Idle.refusal());
        }
        state.touched = now;
        Ok(())
    }

    fn failed_on<T>(&self, written: Result<T>) -> Result<T> {
        failed(&self.state, written)
    }

    fn dropped_on<T>(&self, found: Result<T>) -> Result<T> {
        if found.is_err() {
            drop_table(&self.db, &self.state);
        }
        found
    }
}

impl Drop for Staged {
    /// A run that ends before its commit leaves no table on the writer.
    fn drop(&mut self) {
        let mut state = lock(&self.state);
        if matches!(state.ended, Some(Ended::Committed)) {
            return;
        }
        state.ended = Some(Ended::Refused("the run was let go of before its commit".to_owned()));
        drop(state);
        drop_table(&self.db, &self.state);
    }
}

fn waited(state: &State, now: SystemTime) -> bool {
    now.duration_since(state.touched).is_ok_and(|waited| waited > IDLE)
}

/// A write that failed ends the run: what it kept, or the table it made,
/// went with the commit that failed.
fn failed<T>(state: &Mutex<State>, written: Result<T>) -> Result<T> {
    if let Err(error) = &written {
        let refused = format!("a part failed, and ended the run: {error}");
        lock(state).ended.get_or_insert(Ended::Refused(refused));
    }
    written
}

/// Keeps a part's rows in the run's table, made by the first part that has
/// rows; every part after gives the same columns.
fn keep(connection: &Connection, state: &Mutex<State>, listed: Listed) -> Result<()> {
    if listed.rows.is_empty() {
        return Ok(());
    }
    let mut state = lock(state);
    if let Some(ended @ (Ended::Refused(_) | Ended::Idle)) = &state.ended {
        return Err(ended.refusal());
    }
    let rows = state.rows + listed.rows.len() as u64;
    if rows > MIRROR_ROWS.bound {
        let why = format!("a run of {rows} rows, past {}: mirror a narrower scope", MIRROR_ROWS.bound);
        return Err(MIRROR_ROWS.refuse(rows, why));
    }
    match &state.columns {
        Some(columns) if *columns != listed.columns => {
            return Err(Error::invalid("a part's columns are not the first part's: give rows of one shape"));
        }
        Some(_) => {}
        None => make_table(connection, &state.table, listed.columns.len())?,
    }
    insert_rows(connection, &state.table, &listed)?;
    (state.columns, state.rows) = (Some(listed.columns), rows);
    Ok(())
}

fn make_table(connection: &Connection, table: &str, width: usize) -> Result<()> {
    let columns: Vec<String> = (0..width).map(|at| format!("c{at}")).collect();
    let text = format!("create temp table {} ({})", table.trim_start_matches("temp."), columns.join(", "));
    connection.execute_batch(&text).map_err(|error| failed_sql("its run's table", error))
}

fn insert_rows(connection: &Connection, table: &str, listed: &Listed) -> Result<()> {
    let marks = vec!["?"; listed.columns.len()].join(", ");
    let mut insert = connection
        .prepare(&format!("insert into {table} values ({marks})"))
        .map_err(|error| failed_sql("its run's table", error))?;
    for row in &listed.rows {
        insert.execute(rusqlite::params_from_iter(row)).map_err(|error| failed_sql("its run's table", error))?;
    }
    Ok(())
}

/// The run's parts as one list, in the order they came, within the store's
/// memory and one call's bound.
fn read_back(connection: &Connection, state: &Mutex<State>, memory: &Arc<crate::Memory>) -> Result<(Listed, Held)> {
    let state = lock(state);
    let mut held = Held::of(memory);
    let Some(columns) = state.columns.clone() else { return Ok((Listed::default(), held)) };
    let mut read = connection
        .prepare(&format!("select * from {} order by rowid", state.table))
        .map_err(|error| failed_sql("its run's table", error))?;
    let mut found = read.query([]).map_err(|error| failed_sql("its run's table", error))?;
    let (mut rows, mut bytes) = (Vec::new(), 0);
    while let Some(row) = found.next().map_err(|error| failed_sql("its run's table", error))? {
        let values = (0..columns.len()).map(|at| row.get_ref(at).map(Value::of));
        let values =
            values.collect::<rusqlite::Result<Vec<_>>>().map_err(|error| failed_sql("its run's table", error))?;
        bytes += values.iter().map(Value::size).sum::<usize>();
        held.cover(bytes)?;
        rows.push(values);
    }
    Ok((Listed { columns, rows }, held))
}

fn drop_on(connection: &Connection, state: &Mutex<State>) -> Result<()> {
    let state = lock(state);
    if state.columns.is_none() {
        return Ok(());
    }
    let text = format!("drop table if exists {}", state.table);
    connection.execute_batch(&text).map_err(|error| failed_sql("its run's table", error))
}

/// Drops the run's table in a write of its own, waiting for nothing: a run
/// that ended unwritten, or whose commit failed.
fn drop_table(db: &Database, state: &Arc<Mutex<State>>) {
    if lock(state).columns.is_none() {
        return;
    }
    let state = Arc::clone(state);
    db.file().submit(0, move |tx: &Tx<'_>| drop_on(tx, &state), |_| {});
}

fn failed_sql(what: &str, error: rusqlite::Error) -> Error {
    crate::sqlite::sql_error(what, error)
}
