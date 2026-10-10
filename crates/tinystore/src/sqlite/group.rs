use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::connection::{execute, sql_error};
use super::{GroupLimits, Tx};
use crate::{Error, ErrorKind, Result};

pub(crate) type Work = Box<dyn FnOnce(&Tx<'_>) -> Result<()> + Send>;

/// What a submitted write calls with its answer, on the thread that commits it.
pub(crate) type Done = Box<dyn FnOnce(Result<()>) + Send>;

/// A leader gathers for at most a quarter of the last commit, counted from
/// its end, and never more than the limits' `gather`.
///
/// The callers a commit answers need tens of microseconds to wake and write
/// again, and a leader that took the queue at once left them all to the next
/// commit. 64 writers then made groups of 32 for good: the half that had
/// waited while the other half's commit ran, each half a commit, half the
/// writes a sync could carry (research rust-slice-2026-10-10). So a leader
/// waits for both halves, the writes that waited when the last commit ended
/// and the ones that commit answered.
const GATHER_SHARE: u32 = 4;

pub(crate) const BEGIN: &str = "begin immediate";
pub(crate) const COMMIT: &str = "commit";
pub(crate) const ROLLBACK: &str = "rollback";
const SAVEPOINT: &str = "savepoint grouped";
const RELEASE: &str = "release grouped";
const ROLLBACK_TO: &str = "rollback to grouped";

/// The writes waiting for a file's writer.
///
/// The first caller to find no leader commits every write queued behind it in
/// one transaction, each write in a savepoint, then hands the lead to the first
/// caller still waiting. A commit's sync is shared by the writes that arrived
/// while the last one ran, and no thread is started: callers take turns.
///
/// A submitted write has no caller waiting: it is answered through its `done`,
/// on the leader's thread, and a leader that finds one at the front of the
/// queue leads on, since nobody else would. So a host with one thread and many
/// writes in flight, an event loop, fills a commit without a thread a write.
///
/// research's design/group-commit-contract.md is the contract.
pub(crate) struct Group {
    queue: Mutex<Queue>,
    grew: Condvar,
    limits: GroupLimits,
    commits: AtomicU64,
}

#[derive(Default)]
struct Queue {
    waiting: VecDeque<Arc<Write>>,
    leading: bool,
    closed: bool,
    /// How many writes a gathering leader waits for: the write that makes
    /// them wakes it, and no other, since a wake a write would cost each
    /// caller more than its write.
    gathering: Option<usize>,
    /// The writes the next commit may expect: those that waited when the last
    /// commit ended, and the ones it answered, whose callers may be about to
    /// write again.
    expected: usize,
    last_held: Duration,
    last_ended: Option<Instant>,
}

/// One caller's write, from its queueing to its answer.
struct Write {
    bytes: usize,
    work: Mutex<Option<Work>>,
    /// A submitted write's answer goes here rather than to a waiting caller.
    done: Mutex<Option<Done>>,
    turn: Mutex<Turn>,
    changed: Condvar,
}

enum Turn {
    Waiting,
    Leading,
    /// Taken once, by the caller.
    Answered(Option<Answer>),
}

/// A write's answer, and the answers its caller passes on as it returns.
struct Answer {
    own: Result<()>,
    passed_on: Vec<(Arc<Write>, Result<()>)>,
}

impl Group {
    pub(crate) fn new(limits: GroupLimits) -> Self {
        Self { queue: Mutex::new(Queue::default()), grew: Condvar::new(), limits, commits: AtomicU64::new(0) }
    }

    pub(crate) fn commits(&self) -> u64 {
        self.commits.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.lock().waiting.len()
    }

    /// Runs `work` in a savepoint of a transaction it may share with the writes
    /// queued beside it, and returns once that transaction commits.
    ///
    /// A failed or panicking write rolls back its savepoint and fails alone. A
    /// write heavier than the group's bytes commits alone. A failed commit fails
    /// every write that ran in it with `OutcomeUnknown`.
    pub(crate) fn write(&self, writer: &Mutex<Connection>, bytes: usize, work: Work) -> Result<()> {
        let write = Arc::new(Write::new(bytes, work));
        let leads = self.enqueue(&write)?;
        if leads || write.wait_for_turn() {
            self.lead(writer, Some(&write));
        }
        write.take_answer()
    }

    /// Queues `work` and returns at once, `done` called with its answer once
    /// its commit ends. When no commit runs, this caller's thread commits the
    /// queue first, as long as submitted writes keep arriving at its front.
    pub(crate) fn submit(&self, writer: &Mutex<Connection>, bytes: usize, work: Work, done: Done) {
        let write = Arc::new(Write::submitted(bytes, work, done));
        match self.enqueue(&write) {
            Ok(true) => self.lead(writer, None),
            Ok(false) => {}
            Err(error) => write.answer(Err(error), Vec::new()),
        }
    }

    /// Refuses new writes and answers those still waiting; a commit under way
    /// finishes and answers its own.
    pub(crate) fn close(&self) {
        let waiting = {
            let mut queue = self.lock();
            queue.closed = true;
            std::mem::take(&mut queue.waiting)
        };
        for write in waiting {
            write.answer(Err(Error::closed("a grouped write")), Vec::new());
        }
        self.grew.notify_all();
    }

    /// Queues a write and says whether its caller leads.
    fn enqueue(&self, write: &Arc<Write>) -> Result<bool> {
        let mut queue = self.lock();
        if queue.closed {
            return Err(Error::closed("a grouped write"));
        }
        queue.waiting.push_back(Arc::clone(write));
        if queue.leading {
            if queue.gathering.is_some_and(|wanted| queue.waiting.len() >= wanted) {
                self.grew.notify_all();
            }
            return Ok(false);
        }
        queue.leading = true;
        Ok(true)
    }

    /// Commits batches until the write at the front has a caller of its own to
    /// lead, or the queue is empty. `own` is the write of the caller that
    /// leads, when it has one.
    fn lead(&self, writer: &Mutex<Connection>, own: Option<&Arc<Write>>) {
        loop {
            let connection = writer.lock().unwrap_or_else(PoisonError::into_inner);
            let batch = self.gather();
            let started = Instant::now();
            let answers = commit(&connection, &batch);
            let held = started.elapsed();
            drop(connection);
            if !batch.is_empty() {
                self.commits.fetch_add(1, Ordering::Relaxed);
            }
            let leads_on = self.hand_off(batch.len(), held);
            answer_all(batch, answers, own);
            if !leads_on {
                return;
            }
        }
    }

    /// Waits a little for the writes the last commit lets it expect, then
    /// takes a batch from the front of the queue. A leader that comes long
    /// after that commit waits for nothing: its callers are not coming back
    /// together.
    fn gather(&self) -> Vec<Arc<Write>> {
        let mut queue = self.lock();
        let wanted = queue.expected.min(self.limits.writes);
        let window = (queue.last_held / GATHER_SHARE).min(self.limits.gather);
        let deadline = queue.last_ended.map_or_else(Instant::now, |ended| ended + window);
        queue.gathering = Some(wanted);
        while queue.waiting.len() < wanted && !queue.closed {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            queue = self.grew.wait_timeout(queue, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        queue.gathering = None;
        self.take(&mut queue)
    }

    fn take(&self, queue: &mut Queue) -> Vec<Arc<Write>> {
        let mut batch = Vec::new();
        let mut bytes = 0usize;
        while let Some(next) = queue.waiting.front() {
            let full = batch.len() == self.limits.writes || bytes.saturating_add(next.bytes) > self.limits.bytes;
            if full && !batch.is_empty() {
                break;
            }
            bytes = bytes.saturating_add(next.bytes);
            batch.extend(queue.waiting.pop_front());
        }
        batch
    }

    /// Passes the lead to the caller of the write at the front, and says
    /// whether this thread leads on because that write was submitted.
    fn hand_off(&self, answered: usize, held: Duration) -> bool {
        let mut queue = self.lock();
        queue.expected = queue.waiting.len() + answered;
        queue.last_held = held;
        queue.last_ended = Some(Instant::now());
        match queue.waiting.front() {
            Some(next) if next.is_submitted() => true,
            Some(next) => {
                next.promote();
                false
            }
            None => {
                queue.leading = false;
                false
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Write {
    fn new(bytes: usize, work: Work) -> Self {
        let (done, turn) = (Mutex::new(None), Mutex::new(Turn::Waiting));
        Self { bytes, work: Mutex::new(Some(work)), done, turn, changed: Condvar::new() }
    }

    fn submitted(bytes: usize, work: Work, done: Done) -> Self {
        Self { done: Mutex::new(Some(done)), ..Self::new(bytes, work) }
    }

    fn is_submitted(&self) -> bool {
        self.done.lock().unwrap_or_else(PoisonError::into_inner).is_some()
    }

    /// Blocks until the write leads or is answered; true when it leads.
    fn wait_for_turn(&self) -> bool {
        let mut turn = self.lock_turn();
        loop {
            match *turn {
                Turn::Waiting => turn = self.changed.wait(turn).unwrap_or_else(PoisonError::into_inner),
                Turn::Leading => return true,
                Turn::Answered(_) => return false,
            }
        }
    }

    fn promote(&self) {
        let mut turn = self.lock_turn();
        if matches!(*turn, Turn::Waiting) {
            *turn = Turn::Leading;
            self.changed.notify_one();
        }
    }

    /// Answers the write, and gives its caller the answers it passes on. A
    /// submitted write has no caller to pass any on, and is given none.
    fn answer(&self, own: Result<()>, passed_on: Vec<(Arc<Write>, Result<()>)>) {
        let done = self.done.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(done) = done {
            return done(own);
        }
        *self.lock_turn() = Turn::Answered(Some(Answer { own, passed_on }));
        self.changed.notify_one();
    }

    fn take_work(&self) -> Option<Work> {
        self.work.lock().unwrap_or_else(PoisonError::into_inner).take()
    }

    /// The write's answer, once the answers given with it are passed on.
    fn take_answer(&self) -> Result<()> {
        let answer = match &mut *self.lock_turn() {
            Turn::Answered(answer) => answer.take(),
            _ => return Err(Error::internal("a grouped write ended without an answer")),
        };
        let Some(Answer { own, passed_on }) = answer else {
            return Err(Error::internal("a grouped write was answered twice"));
        };
        answer_in_halves(passed_on);
        own
    }

    fn lock_turn(&self) -> MutexGuard<'_, Turn> {
        self.turn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Answers a commit's writes: a submitted one through its `done`, here, and
/// the callers that wait by waking them.
///
/// The writer does nothing a caller at a time. A wake costs tens of
/// microseconds where the processor it lands on sleeps, and a leader that
/// woke 64 callers one after another spent on it what their commit had
/// taken, so that half of them missed the next commit (research
/// rust-slice-2026-10-10). So the callers wake one another, in halves. The
/// leader's own write is given none to pass on, since its thread may lead
/// the next commit first.
fn answer_all(batch: Vec<Arc<Write>>, answers: Vec<Result<()>>, own: Option<&Arc<Write>>) {
    let mut callers = Vec::with_capacity(batch.len());
    for (write, answer) in batch.into_iter().zip(answers) {
        if write.is_submitted() || own.is_some_and(|own| Arc::ptr_eq(own, &write)) {
            write.answer(answer, Vec::new());
        } else {
            callers.push((write, answer));
        }
    }
    answer_in_halves(callers);
}

/// Answers callers with two wakes, however many they are: each half through
/// its first, whose caller answers the rest of its half the same way as it
/// returns. An answer travels with its wake, so no caller wakes to find none,
/// and the last is awake after as many wakes in a row as the callers halve
/// to one, six for 64.
fn answer_in_halves(mut callers: Vec<(Arc<Write>, Result<()>)>) {
    let second = callers.split_off(callers.len().div_ceil(2));
    for half in [callers, second] {
        let mut half = half.into_iter();
        if let Some((first, answer)) = half.next() {
            first.answer(answer, half.collect());
        }
    }
}

/// Runs a batch in one transaction, each write in a savepoint, and says how
/// each one ended.
fn commit(connection: &Connection, batch: &[Arc<Write>]) -> Vec<Result<()>> {
    if batch.is_empty() {
        return Vec::new();
    }
    if let Err(error) = execute(connection, BEGIN) {
        let failure = sql_error("a grouped commit: its transaction", error);
        return batch.iter().map(|_| Err(failure.duplicate())).collect();
    }
    let answers: Vec<Result<()>> = batch.iter().map(|write| run(connection, write.take_work())).collect();
    let Err(error) = execute(connection, COMMIT) else {
        return answers;
    };
    if !connection.is_autocommit() {
        // The commit failed and left the transaction open: nothing of it stays.
        let _ = execute(connection, ROLLBACK);
    }
    let failure = sql_error("a grouped commit", error);
    let unknown = || Error::new(ErrorKind::OutcomeUnknown, failure.to_string());
    answers.into_iter().map(|answer| answer.and_then(|()| Err(unknown()))).collect()
}

/// Runs one write in a savepoint, which it keeps when the write succeeds and
/// rolls back when it fails or panics.
fn run(connection: &Connection, work: Option<Work>) -> Result<()> {
    let Some(work) = work else {
        return Err(Error::internal("a grouped write ran twice"));
    };
    execute(connection, SAVEPOINT).map_err(|error| sql_error("a grouped write: its savepoint", error))?;
    let tx = Tx::new(connection);
    let outcome = catch_unwind(AssertUnwindSafe(|| work(&tx)))
        .unwrap_or_else(|_| Err(Error::internal("a grouped write panicked")));
    if outcome.is_ok() {
        if let Err(error) = execute(connection, RELEASE) {
            undo(connection);
            return Err(sql_error("a grouped write: its savepoint", error));
        }
        return Ok(());
    }
    undo(connection);
    outcome
}

fn undo(connection: &Connection) {
    // A savepoint that cannot be rolled back leaves the transaction failing at
    // its commit, which answers every write of the batch.
    let _ = execute(connection, ROLLBACK_TO).and_then(|()| execute(connection, RELEASE));
}
