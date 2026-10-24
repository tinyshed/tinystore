use std::cell::Cell;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::capture::{Capture, Logged, in_the_file, logged};
use super::checkpoint::Checkpoints;
use super::connection::{self, execute, sql_error};
use super::given::own;
use super::{Committed, Config, GroupLimits, Left, Settlers, Tx};
use crate::probe::FileProbe;
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

pub(crate) const BEGIN: &str = own!("begin immediate");
pub(crate) const COMMIT: &str = own!("commit");
pub(crate) const ROLLBACK: &str = own!("rollback");
const SAVEPOINT: &str = own!("savepoint grouped");
const RELEASE: &str = own!("release grouped");
const ROLLBACK_TO: &str = own!("rollback to grouped");

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
/// A write submitted from an executor's thread, inside `polled::leading_on`,
/// hands the lead it would take to the store's pool instead, or to a thread
/// of its own once the pool stopped, so that no sync of the disk runs where
/// futures are polled. Until a thread of the pool takes
/// it, the next caller who can lead takes it first: a pool whose every thread
/// waits for this file's writer would never run the lead they wait for.
///
/// A checkpoint a commit asked for runs on a caller's thread once it holds no
/// turn: when its call returns, or when it queued a write behind a leader.
///
/// research's design/group-commit-contract.md is the contract.
pub(crate) struct Group {
    queue: Mutex<Queue>,
    grew: Condvar,
    limits: GroupLimits,
    commits: AtomicU64,
    settlers: Arc<Settlers>,
    capture: Logged,
    checkpoints: Arc<Checkpoints>,
    probe: Option<FileProbe>,
}

#[derive(Default)]
struct Queue {
    waiting: VecDeque<Arc<Write>>,
    leading: bool,
    /// The lead was handed to another thread that has not taken it yet.
    handed: bool,
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
    /// When it was queued, if it is measured.
    queued: Option<Instant>,
    /// Queued by a caller who waits for it, not by the store's own work.
    for_a_caller: bool,
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
    pub(crate) fn new(
        config: &Config,
        settlers: Arc<Settlers>,
        capture: Logged,
        checkpoints: Arc<Checkpoints>,
    ) -> Self {
        Self {
            queue: Mutex::new(Queue::default()),
            grew: Condvar::new(),
            limits: config.group,
            commits: AtomicU64::new(0),
            settlers,
            capture,
            checkpoints,
            probe: config.probe.clone(),
        }
    }

    #[cfg(test)]
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
        let write = Arc::new(Write::new(bytes, work, self.queued()));
        let leads = self.enqueue(&write, false)?;
        if leads || write.wait_for_turn() {
            self.lead(writer, Some(&write));
        }
        let answer = write.take_answer();
        self.checkpoints.run_due();
        answer
    }

    /// Queues `work` and returns at once, `done` called with its answer once
    /// its commit ends. When no commit runs, this caller's thread commits the
    /// queue first, as long as submitted writes keep arriving at its front;
    /// inside `polled::leading_on`, a thread of the pool does instead.
    pub(crate) fn submit(self: &Arc<Self>, writer: &Arc<Mutex<Connection>>, bytes: usize, work: Work, done: Done) {
        let write = Arc::new(Write::submitted(bytes, work, done, self.queued()));
        let elsewhere = crate::polled::lead();
        match (self.enqueue(&write, elsewhere.is_some()), elsewhere) {
            (Err(error), _) => return write.answer(Err(error), Vec::new()),
            // the lead, and the checkpoints after it, run where the scope says
            (Ok(true), Some(lead)) => return lead(Handed::job(Arc::clone(self), Arc::clone(writer))),
            (Ok(false), Some(_)) => return,
            (Ok(true), None) => self.lead(writer, None),
            (Ok(false), None) => {}
        }
        // A leader on an event loop's writes leads on while they come, so the
        // threads that queue them behind it run what its commits asked for.
        self.checkpoints.run_due();
    }

    fn queued(&self) -> Option<Instant> {
        self.probe.as_ref().and_then(FileProbe::queued)
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

    /// Queues a write and says whether its caller leads: one that will hand
    /// the lead `elsewhere` marks it handed, and one that will not takes a
    /// lead handed that no thread took yet.
    fn enqueue(&self, write: &Arc<Write>, elsewhere: bool) -> Result<bool> {
        let mut queue = self.lock();
        if queue.closed {
            return Err(Error::closed("a grouped write"));
        }
        queue.waiting.push_back(Arc::clone(write));
        if queue.leading {
            if queue.handed && !elsewhere {
                queue.handed = false;
                return Ok(true);
            }
            if queue.gathering.is_some_and(|wanted| queue.waiting.len() >= wanted) {
                self.grew.notify_all();
            }
            return Ok(false);
        }
        queue.leading = true;
        queue.handed = elsewhere;
        Ok(true)
    }

    /// Takes the lead handed to another thread, unless a caller took it
    /// first; says whether this thread leads.
    fn take_handed(&self) -> bool {
        std::mem::take(&mut self.lock().handed)
    }

    /// Commits batches until the write at the front has a caller of its own to
    /// lead, or the queue is empty. `own` is the write of the caller that
    /// leads, when it has one.
    fn lead(&self, writer: &Mutex<Connection>, own: Option<&Arc<Write>>) {
        loop {
            let connection = writer.lock().unwrap_or_else(PoisonError::into_inner);
            let batch = self.gather();
            let started = Instant::now();
            // read while the writer is held, which a change of the file's home holds too
            let capture = logged(&self.capture);
            // A panic past the writes' own savepoints, in a settler or a hook,
            // must still reach `hand_off`, or no thread leads again.
            let answers =
                catch_unwind(AssertUnwindSafe(|| self.commit(&connection, &batch, capture.as_deref(), started)))
                    .unwrap_or_else(|_| {
                        if !connection.is_autocommit() {
                            let _ = execute(&connection, ROLLBACK);
                        }
                        let panicked = Error::new(ErrorKind::OutcomeUnknown, "a grouped commit panicked");
                        batch.iter().map(|_| Err(panicked.duplicate())).collect()
                    });
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

/// A commit's lead handed to another thread. It leads wherever it ends: on
/// the thread that runs it, or, dropped unrun where no thread could be
/// started for it, on the thread that drops it, since the writes queued
/// behind the lead have no other leader. A caller that took the lead first
/// leaves it nothing to do.
struct Handed {
    group: Arc<Group>,
    writer: Arc<Mutex<Connection>>,
}

impl Handed {
    fn job(group: Arc<Group>, writer: Arc<Mutex<Connection>>) -> crate::polled::Job {
        let handed = Handed { group, writer };
        Box::new(move || drop(handed))
    }
}

impl Drop for Handed {
    fn drop(&mut self) {
        if self.group.take_handed() {
            self.group.lead(&self.writer, None);
            self.group.checkpoints.run_due();
        }
    }
}

impl Write {
    fn new(bytes: usize, work: Work, queued: Option<Instant>) -> Self {
        let (done, turn) = (Mutex::new(None), Mutex::new(Turn::Waiting));
        let for_a_caller = !super::capture::is_own_work();
        Self { bytes, queued, for_a_caller, work: Mutex::new(Some(work)), done, turn, changed: Condvar::new() }
    }

    fn submitted(bytes: usize, work: Work, done: Done, queued: Option<Instant>) -> Self {
        Self { done: Mutex::new(Some(done)), ..Self::new(bytes, work, queued) }
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

/// How long each measured write of a batch waited for the commit that began
/// at `started`.
fn waits(batch: &[Arc<Write>], started: Instant) -> Vec<Duration> {
    batch.iter().filter_map(|write| write.queued).map(|queued| started.saturating_duration_since(queued)).collect()
}

/// What becomes of each write of a batch.
type Answers = Vec<Result<()>>;

impl Group {
    /// Runs a batch, gathered at `started`, and says how each write ended.
    ///
    /// The batch commits as one transaction, unless a write ends it: SQLite
    /// then rolls the whole of it back, as it does for a statement's `or
    /// rollback`, and the writes after that one, which have not run yet, go on
    /// in the next transaction.
    fn commit(
        &self,
        connection: &Connection,
        batch: &[Arc<Write>],
        capture: Option<&Capture>,
        started: Instant,
    ) -> Answers {
        let mut answers = Answers::with_capacity(batch.len());
        while answers.len() < batch.len() {
            let rest = &batch[answers.len()..];
            answers.extend(self.commit_part(connection, rest, capture, &waits(rest, started)));
        }
        answers
    }

    /// Runs writes from the front of `batch` in one transaction, each write in
    /// a savepoint, and says how each one that ran ended: every one, or those
    /// up to the one that ended the transaction. What the writes that
    /// succeeded asked to run once they commit runs after the commit, in their
    /// order, the writer still held. A commit that holds measured writes,
    /// whose `waits` are given, is told to the probe.
    fn commit_part(
        &self,
        connection: &Connection,
        batch: &[Arc<Write>],
        capture: Option<&Capture>,
        waits: &[Duration],
    ) -> Answers {
        let committed = Committed::default();
        let answers = match self.run_batch(connection, batch, capture, &committed) {
            Ok(answers) => answers,
            Err(every_one) => return every_one,
        };
        match capture {
            Some(capture) => capture.arm(batch.iter().any(|write| write.for_a_caller)),
            None => self.checkpoints.arm(),
        }
        let started = Instant::now();
        let finished = connection::commit(connection);
        if let (Some(probe), true) = (&self.probe, finished.is_ok()) {
            probe.committed(started.elapsed(), waits);
        }
        let Err(error) = finished else {
            return answered(batch, answers, self.handed(connection, capture, &committed));
        };
        if !connection.is_autocommit() {
            // The commit failed and left the transaction open: nothing of it stays.
            let _ = execute(connection, ROLLBACK);
        }
        let failure = sql_error("a grouped commit", error);
        let unknown = || Error::new(ErrorKind::OutcomeUnknown, "a grouped commit").with_source(failure.duplicate());
        answers.into_iter().map(|answer| answer.and_then(|()| Err(unknown()))).collect()
    }

    /// Begins the batch's transaction and runs its writes, then settles what
    /// they left together, once, after the last: how each write ended, or,
    /// once the transaction rolled back, the answers of the writes that ran in
    /// it, every one of the batch or those up to the one that ended it.
    fn run_batch(
        &self,
        connection: &Connection,
        batch: &[Arc<Write>],
        capture: Option<&Capture>,
        committed: &Committed,
    ) -> std::result::Result<Answers, Answers> {
        let every_one = |failure: Error| batch.iter().map(|_| Err(failure.duplicate())).collect::<Answers>();
        if let Some(Err(refused)) = capture.map(Capture::refuse_a_write) {
            return Err(every_one(refused));
        }
        execute(connection, BEGIN).map_err(|error| every_one(sql_error("a grouped commit: its transaction", error)))?;
        let tx = Tx::new(connection, committed, &self.settlers);
        if let Err(failure) = tx.settle(Left::Before) {
            let _ = execute(connection, ROLLBACK);
            return Err(every_one(failure));
        }
        let undone = Cell::new(true);
        let mut answers = Answers::with_capacity(batch.len());
        for write in batch {
            let answer = run(connection, write.take_work(), (committed, &self.settlers), &undone);
            if connection.is_autocommit() {
                return Err(ended_by(answers, answer));
            }
            answers.push(answer);
        }
        // A failed write whose savepoint would not roll back is still in the
        // transaction: committing would keep it, so the whole batch goes.
        if !undone.get() {
            let _ = execute(connection, ROLLBACK);
            return Err(every_one(Error::internal(
                "a grouped write's savepoint did not roll back; its batch rolled back",
            )));
        }
        if let Err(failure) = tx.settle(Left::All).and_then(|()| still_open(connection)) {
            let _ = execute(connection, ROLLBACK);
            return Err(answers.into_iter().map(|answer| answer.and_then(|()| Err(failure.duplicate()))).collect());
        }
        Ok(answers)
    }

    /// Hands a commit that is in the file to the capture, where the file has
    /// one, or tells the file's checkpoints where it left the WAL, then runs
    /// what its writes asked to run once it is there.
    fn handed(&self, connection: &Connection, capture: Option<&Capture>, committed: &Committed) -> Result<()> {
        let handed = match capture {
            Some(capture) => capture.committed(connection),
            None => {
                self.checkpoints.committed(connection);
                Ok(())
            }
        };
        if in_the_file(&handed) {
            committed.run();
        }
        handed
    }
}

/// The answers of a batch whose commit is in the file, as the capture says
/// it `handed` the commit on.
fn answered(batch: &[Arc<Write>], answers: Answers, handed: Result<()>) -> Answers {
    let Err(handed) = handed else {
        return answers;
    };
    // the store's own writes take a commit no copy held as done, as they never waited for one
    let unheld = handed.kind() == ErrorKind::CommittedLocally;
    let answer_of = |(write, answer): (&Arc<Write>, Result<()>)| match answer {
        Ok(()) if unheld && !write.for_a_caller => Ok(()),
        answer => answer.and_then(|()| Err(handed.duplicate())),
    };
    batch.iter().zip(answers).map(answer_of).collect()
}

/// The answers of the writes of a transaction that the last of them, whose
/// own answer is `ending`, ended: SQLite rolled it back whole. A write before
/// it that had not failed alone may succeed again; the one that ended it
/// fails, with its own failure when it has one.
fn ended_by(before: Answers, ending: Result<()>) -> Answers {
    let rolled_back = || {
        let why =
            "a write beside it in its commit ended the transaction, which rolled back whole: it may succeed again";
        Error::new(ErrorKind::Unavailable, why)
    };
    let ending = ending.and_then(|()| Err(Error::internal("a grouped write ended its transaction and did not fail")));
    let ending = ending.map_err(|error| error.within("it ended the transaction its commit shared with other writes"));
    before.into_iter().map(|answer| answer.and_then(|()| Err(rolled_back()))).chain([ending]).collect()
}

/// Fails once SQLite rolled the transaction back whole, so that nothing more
/// runs on the writer as though it had not.
fn still_open(connection: &Connection) -> Result<()> {
    if connection.is_autocommit() {
        return Err(Error::internal("the transaction rolled back before its commit"));
    }
    Ok(())
}

/// Runs one write in a savepoint, then settles what it left, keeping the
/// savepoint when both succeed and rolling it back when either fails or
/// panics, its hooks with it. A write that ended the transaction leaves its
/// batch to answer for it.
fn run(
    connection: &Connection,
    work: Option<Work>,
    (committed, settlers): (&Committed, &Settlers),
    undone: &Cell<bool>,
) -> Result<()> {
    let Some(work) = work else {
        return Err(Error::internal("a grouped write ran twice"));
    };
    execute(connection, SAVEPOINT).map_err(|error| sql_error("a grouped write: its savepoint", error))?;
    let mark = committed.mark();
    let tx = Tx::new(connection, committed, settlers);
    let settled = || work(&tx).and_then(|()| still_open(connection)).and_then(|()| tx.settle(Left::Here));
    let outcome =
        catch_unwind(AssertUnwindSafe(settled)).unwrap_or_else(|_| Err(Error::internal("a grouped write panicked")));
    if connection.is_autocommit() {
        return outcome;
    }
    if outcome.is_ok() {
        if let Err(error) = execute(connection, RELEASE) {
            undone.set(undone.get() && undo(connection));
            committed.take_back(mark);
            return Err(sql_error("a grouped write: its savepoint", error));
        }
        return Ok(());
    }
    undone.set(undone.get() && undo(connection));
    committed.take_back(mark);
    outcome
}

/// Rolls a write's savepoint back; says whether it did.
fn undo(connection: &Connection) -> bool {
    execute(connection, ROLLBACK_TO).and_then(|()| execute(connection, RELEASE)).is_ok()
}
