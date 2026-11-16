//! A walk of a statement's rows, `each`: read a part at a time from one
//! statement on one reader, while the program takes the parts read before, so
//! that memory holds a few parts and not the rows (SQL-13).
//!
//! A part is read on the store's pool, or on the thread that asked over the
//! wire, and decoded by whoever takes it, outside the read: the snapshot
//! covers fetching the bytes alone. The reader waits for room for its next
//! part, two parts ahead of the program at most:
//!
//! ```text
//! reader: [part 3, being read] → [part 2][part 1], waiting → program: part 0, a row at a time
//! ```
//!
//! A snapshot is the oldest the write-ahead log keeps, so a walk holds one
//! for [`BOUND`] at most. Ordered by a unique key, it reads on past that in a
//! snapshot of its own, after the last row it gave, as `list` pages; any other
//! walk fails `limit` there, naming `list`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, Statement, params_from_iter};

use super::database::Database;
use super::query::{Direction, read_on};
use super::rows::{Held, failure, named_columns};
use super::statement::Sql;
use super::values::Value;
use crate::limits::Limit;
use crate::pool::Pool;
use crate::sqlite::given;
use crate::{Error, Result};

/// How long a walk holds one snapshot at most, by the store's clock, as
/// `docs/handbook/production/limits.md` says of one `each` read.
pub(crate) const BOUND: Duration = Duration::from_secs(5);
/// [`BOUND`] as a refusal names it, in milliseconds.
pub(crate) const HELD: Limit = Limit { name: "snapshot millis", bound: BOUND.as_millis() as u64 };

/// The parts a walk keeps read ahead of its program: one for the program to
/// take while the next is read.
const AHEAD: usize = 2;

/// How long a reader waiting for room sleeps before it reads the store's
/// clock again: a test's clock moves without waking it.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// A column a walk's rows are ordered by, named as the rows name it.
#[derive(Clone, Debug)]
pub(crate) struct Key {
    pub(crate) column: String,
    pub(crate) direction: Direction,
}

/// What a walk reads its rows into, a part at a time: the program's rows, or
/// the wire's message.
pub(crate) trait Part: Send + 'static {
    /// An empty part of rows of `columns`, of about `bound` bytes; the walk's
    /// first names its columns.
    fn begin(columns: &Arc<[String]>, first: bool, bound: usize, held: Held) -> Self;

    /// What a row takes of a part.
    fn weight(row: &[Value]) -> usize;

    /// Adds a row of `weight`, within the store's memory; a row no part
    /// takes is `limit`.
    fn push(&mut self, row: Vec<Value>, weight: usize) -> Result<()>;

    /// The weight of its rows.
    fn bytes(&self) -> usize;

    /// Makes the part what its taker reads, once its last row is in.
    fn seal(&mut self) {}
}

/// The statement a walk reads, and how.
pub(crate) struct Walking {
    pub(crate) database: Database,
    pub(crate) statement: Sql,
    /// The columns its rows are ordered by, a unique one last, when it says
    /// them: the walk then reads on past one snapshot.
    pub(crate) key: Option<Vec<Key>>,
    /// The bytes of a part.
    pub(crate) bound: usize,
}

/// How a walk ended, after its parts.
#[derive(Debug)]
pub(crate) enum End {
    Done,
    Failed(Error),
    /// The statement writes: it is no walk, and its rows come as `all` reads
    /// them.
    Writes,
}

/// What a taker finds.
pub(crate) enum Taken<P> {
    Part(P),
    End(End),
    /// Nothing yet: the reader calls back once a part or the end comes.
    Pending,
}

/// A walk's first answer, as [`Walk::first`] finds it.
#[cfg(feature = "wire")]
pub(crate) enum First<P> {
    Whole(Option<P>),
    End(End),
    Parts,
}

/// The program's end of a walk: the parts as they are read. Dropped, it
/// stops the reader at its next row, which gives its reader back.
pub(crate) struct Walk<P: Part> {
    shared: Arc<Shared<P>>,
}

/// The reading end of a walk, which runs it: on this thread, or on the pool.
pub(crate) struct Reader<P: Part> {
    shared: Arc<Shared<P>>,
    after: Option<Vec<Value>>,
    first: bool,
    /// Whether it ran: one the pool dropped unrun, as its store closed, ends
    /// the walk as closed rather than leave its program waiting for ever.
    ran: bool,
}

struct Shared<P> {
    walking: Walking,
    pool: Arc<Pool>,
    state: Mutex<State<P>>,
    changed: Condvar,
    left: AtomicBool,
}

type Wake = Arc<dyn Fn() + Send + Sync>;

/// A walk's columns, and where among them its key's columns are.
type Named = (Arc<[String]>, Option<Vec<usize>>);

struct State<P> {
    parts: VecDeque<P>,
    end: Option<End>,
    /// Where the walk reads on once the program takes a part: a snapshot
    /// ended, and found no room to begin the next.
    resume: Option<Vec<Value>>,
    /// Called once a part or the end comes: an awaited walk's waker, or the
    /// wire's sending.
    wake: Option<Wake>,
    taken_end: bool,
}

/// How a snapshot of a walk ended.
enum Cut {
    Done,
    Writes,
    /// Its time passed: the walk reads on after these values of its key.
    At(Vec<Value>),
    Left,
}

impl<P: Part> Walk<P> {
    /// A walk of `walking`'s rows and its reader, which runs nothing yet.
    pub(crate) fn new(walking: Walking) -> Result<(Walk<P>, Reader<P>)> {
        let pool = Pool::of(&walking.database.store()?)?;
        let state = State { parts: VecDeque::new(), end: None, resume: None, wake: None, taken_end: false };
        let changed = Condvar::new();
        let shared =
            Arc::new(Shared { walking, pool, state: Mutex::new(state), changed, left: AtomicBool::new(false) });
        let reader = Reader { shared: Arc::clone(&shared), after: None, first: true, ran: false };
        Ok((Walk { shared }, reader))
    }

    /// The next part or the end, waiting for them.
    pub(crate) fn wait(&self) -> Taken<P> {
        let mut state = self.shared.lock();
        loop {
            match self.shared.take(&mut state, |_| true) {
                (Taken::Pending, _) => {}
                (taken, reader) => {
                    drop(state);
                    if let Some(reader) = reader {
                        reader.spawn();
                    }
                    return taken;
                }
            }
            state = self.shared.changed.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The next part when `takes` takes it, told whether another part or the
    /// end follows it, or the end; `Pending` otherwise, and `wake` is called
    /// once that may have changed. Taken, its `last` says whether it was the
    /// walk's last part.
    #[cfg(any(feature = "wire", feature = "async"))]
    pub(crate) fn take(&self, takes: impl Fn(&P, bool) -> bool, wake: Wake) -> (Taken<P>, bool) {
        let mut state = self.shared.lock();
        let followed = state.parts.len() >= 2 || state.end.is_some();
        let (taken, reader) = self.shared.take(&mut state, |part| takes(part, followed));
        if matches!(taken, Taken::Pending) {
            state.wake = Some(wake);
        }
        let last = state.parts.is_empty() && matches!(state.end, Some(End::Done));
        drop(state);
        if let Some(reader) = reader {
            reader.spawn();
        }
        (taken, last)
    }

    /// What the walk's first answer is, once it is known: its rows whole, in
    /// one part or none, when it read them all; how it failed, before any
    /// part; or that its parts go one after another.
    #[cfg(feature = "wire")]
    pub(crate) fn first(&self) -> Option<First<P>> {
        let mut state = self.shared.lock();
        match (state.parts.len(), &state.end) {
            (0 | 1, Some(End::Done)) => {
                state.taken_end = true;
                Some(First::Whole(state.parts.pop_front()))
            }
            (0, Some(_)) => {
                state.taken_end = true;
                state.end.take().map(First::End)
            }
            (parts, _) if parts >= AHEAD => Some(First::Parts),
            (_, Some(_)) => Some(First::Parts),
            _ => None,
        }
    }

    /// The parts read ahead of the program, waiting to be taken.
    #[cfg(test)]
    pub(crate) fn queued(&self) -> usize {
        self.shared.lock().parts.len()
    }

    /// Whether a snapshot ended where the walk reads on once a part is taken.
    #[cfg(test)]
    pub(crate) fn resuming(&self) -> bool {
        self.shared.lock().resume.is_some()
    }

    /// Whether the walk read its last row, or failed.
    #[cfg(test)]
    pub(crate) fn ended(&self) -> bool {
        let state = self.shared.lock();
        state.end.is_some() || state.taken_end
    }

    /// Calls `wake` each time a part or the end comes, until another is given.
    #[cfg(feature = "wire")]
    pub(crate) fn on_change(&self, wake: Wake) {
        self.shared.lock().wake = Some(wake);
    }
}

impl<P: Part> Drop for Walk<P> {
    fn drop(&mut self) {
        self.shared.left.store(true, Ordering::SeqCst);
        let mut state = self.shared.lock();
        state.wake = None;
        let parts = std::mem::take(&mut state.parts);
        drop(state);
        drop(parts);
        self.shared.changed.notify_all();
    }
}

impl<P: Part> Reader<P> {
    /// Runs the walk on the store's pool.
    pub(crate) fn spawn(self) {
        let pool = Arc::clone(&self.shared.pool);
        pool.run(Box::new(move || self.run()));
    }

    /// Reads the walk's rows on this thread, a snapshot after another, until
    /// it read them all, failed, or found no room to begin a snapshot: the
    /// program's next take then runs the rest on the pool.
    pub(crate) fn run(mut self) {
        self.ran = true;
        let shared = Arc::clone(&self.shared);
        loop {
            let ended = match shared.snapshot(self.after.as_deref(), &mut self.first) {
                Ok(Cut::Left) => return,
                Ok(Cut::At(after)) => {
                    let mut state = shared.lock();
                    if state.parts.len() < AHEAD {
                        drop(state);
                        self.after = Some(after);
                        continue;
                    }
                    state.resume = Some(after);
                    return;
                }
                Ok(Cut::Done) => End::Done,
                Ok(Cut::Writes) => End::Writes,
                Err(error) => End::Failed(shared.walking.database.failed_by(&shared.walking.statement)(error)),
            };
            return shared.end(ended);
        }
    }
}

impl<P: Part> Drop for Reader<P> {
    fn drop(&mut self) {
        if !self.ran {
            let database = super::engine::described(self.shared.walking.database.name());
            self.shared.end(End::Failed(Error::closed(format!("{database}: a walk of its rows: the store closed"))));
        }
    }
}

impl<P: Part> Shared<P> {
    fn lock(&self) -> MutexGuard<'_, State<P>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes the next part when `takes` takes it, or the end once the parts
    /// are taken; and, once there is room where a snapshot ended, the reader
    /// that reads on, which its caller spawns outside the lock.
    fn take(self: &Arc<Self>, state: &mut State<P>, takes: impl Fn(&P) -> bool) -> (Taken<P>, Option<Reader<P>>) {
        if let Some(part) = state.parts.front() {
            if !takes(part) {
                return (Taken::Pending, None);
            }
            let part = state.parts.pop_front().map_or(Taken::Pending, Taken::Part);
            self.changed.notify_all();
            let after = if state.parts.len() < AHEAD { state.resume.take() } else { None };
            let reader =
                after.map(|after| Reader { shared: Arc::clone(self), after: Some(after), first: false, ran: false });
            return (part, reader);
        }
        if state.taken_end {
            return (Taken::End(End::Done), None);
        }
        match state.end.take() {
            Some(end) => {
                state.taken_end = true;
                (Taken::End(end), None)
            }
            None => (Taken::Pending, None),
        }
    }

    fn end(&self, ended: End) {
        let mut state = self.lock();
        state.end = Some(ended);
        self.woken(state);
    }

    /// Wakes whoever waits for a part, outside the lock: the wire sends from
    /// its waking.
    fn woken(&self, state: MutexGuard<'_, State<P>>) {
        let wake = state.wake.clone();
        drop(state);
        self.changed.notify_all();
        if let Some(wake) = wake {
            wake();
        }
    }

    /// Queues a part once there is room, waiting no later than `deadline`;
    /// past it the part goes in anyway, and the walk ends its snapshot.
    fn push(&self, mut part: P, deadline: SystemTime) {
        part.seal();
        let mut state = self.lock();
        while state.parts.len() >= AHEAD && !self.left() {
            let Ok(left) = deadline.duration_since(self.walking.database.now()) else {
                break;
            };
            let waited = self.changed.wait_timeout(state, left.min(LOOK_AGAIN));
            state = waited.unwrap_or_else(PoisonError::into_inner).0;
        }
        if self.left() {
            return;
        }
        state.parts.push_back(part);
        self.woken(state);
    }

    fn left(&self) -> bool {
        self.left.load(Ordering::SeqCst)
    }

    /// Reads one snapshot of the walk's rows, after `after` when it reads on.
    /// The part it was reading when its rows ended goes once its reader is
    /// given back.
    fn snapshot(&self, after: Option<&[Value]>, first: &mut bool) -> Result<Cut> {
        let walking = &self.walking;
        let deadline = walking.database.now() + BOUND;
        let statement = match (after, &walking.key) {
            (Some(after), Some(key)) => read_on(&walking.statement, key, after)?,
            _ => walking.statement.clone(),
        };
        let read =
            |connection: &Connection| given(statement.text(), || self.step(connection, &statement, deadline, first));
        let (cut, last) = walking.database.file().read(read)?;
        if let Some(last) = last {
            self.push(last, deadline);
        }
        Ok(cut)
    }

    /// Steps the statement, a part after another, until its rows end or its
    /// time passed at a part's end, and gives the part it was reading. Past
    /// forty lines: one loop over a cursor that borrows its statement.
    fn step(
        &self,
        connection: &Connection,
        statement: &Sql,
        deadline: SystemTime,
        first: &mut bool,
    ) -> Result<(Cut, Option<P>)> {
        let mut prepared = connection.prepare_cached(statement.text()).map_err(|error| failure("its text", error))?;
        if !prepared.readonly() {
            return Ok((Cut::Writes, None));
        }
        statement.check(prepared.parameter_count())?;
        let mut rows =
            prepared.query(params_from_iter(statement.values())).map_err(|error| failure("its rows", error))?;
        let (mut named, mut part, mut last_key) = (None, None::<P>, None);
        loop {
            if self.left() {
                return Ok((Cut::Left, None));
            }
            let Some(row) = rows.next().map_err(|error| failure("its rows", error))? else {
                break;
            };
            if named.is_none() {
                named = Some(self.named(row.as_ref())?);
            }
            let Some((columns, at)) = named.as_ref() else { continue };
            let values = (0..columns.len()).map(|index| row.get_ref(index).map(Value::of));
            let values =
                values.collect::<rusqlite::Result<Vec<Value>>>().map_err(|error| failure("its rows", error))?;
            let weight = P::weight(&values);
            if let Some(full) = part.take_if(|part| part.bytes() + weight > self.walking.bound) {
                self.push(full, deadline);
                if self.walking.database.now() > deadline {
                    return Ok((self.cut(at.as_deref(), last_key, &values)?, None));
                }
            }
            let part = part.get_or_insert_with(|| self.begin(columns, first));
            if let Some(at) = at {
                last_key = Some(at.iter().map(|at| values[*at].clone()).collect::<Vec<_>>());
            }
            part.push(values, weight)?;
        }
        drop(rows);
        if part.is_none() && *first {
            // a walk of no rows still names its columns
            part = Some(self.begin(&named_columns(&prepared)?.0.into(), first));
        }
        Ok((Cut::Done, part))
    }

    fn begin(&self, columns: &Arc<[String]>, first: &mut bool) -> P {
        let held = Held::of(self.walking.database.memory());
        let part = P::begin(columns, *first, self.walking.bound, held);
        *first = false;
        part
    }

    /// The rows' columns, and where among them each column of the walk's key
    /// is: none when the walk has no key, or its rows lack one of it.
    fn named(&self, statement: &Statement<'_>) -> Result<Named> {
        let columns: Arc<[String]> = named_columns(statement)?.0.into();
        let at = self.walking.key.as_ref().and_then(|key| {
            key.iter().map(|each| columns.iter().position(|column| *column == each.column)).collect::<Option<Vec<_>>>()
        });
        Ok((columns, at))
    }

    /// Where a walk whose snapshot's time passed goes on: after the last
    /// row's key, when the next row's differs. A walk without a key, or whose
    /// next row shares the last one's, fails `limit`: reading on after it
    /// would skip the rows that share it.
    fn cut(&self, at: Option<&[usize]>, last: Option<Vec<Value>>, next: &[Value]) -> Result<Cut> {
        let (Some(at), Some(last)) = (at, last) else {
            return Err(HELD.run_out(format!(
                "its walk held one snapshot past its {BOUND:?}: order it by a unique key to read on past one \
                 snapshot, or read it a page at a time with list"
            )));
        };
        if at.iter().map(|at| &next[*at]).eq(last.iter()) {
            return Err(HELD.run_out(format!(
                "its walk held one snapshot past its {BOUND:?}, and two rows share its order's values: end the \
                 order with a unique column to read on past one snapshot, or read it a page at a time with list"
            )));
        }
        Ok(Cut::At(last))
    }
}

#[cfg(test)]
#[path = "walk_tests.rs"]
mod tests;
