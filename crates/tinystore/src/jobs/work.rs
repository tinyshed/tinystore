//! Workers: a loop that claims a queue's jobs as they fall due and settles
//! them, and handler threads that run each one.
//!
//! The loop holds up to two jobs a handler, the one it runs and its next, and
//! claims what it lacks in the same grouped write that settles the jobs its
//! handlers finished, so that a queue under load commits once for many jobs and
//! a handler never waits for a commit to start its next.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;

use super::alarm::{LONGEST_SLEEP, Read, Wake};
use super::claim::{self, CLAIM_BATCH, Claimed, Claiming, How, Lease, Settled};
use super::engine::Jobs;
use super::run::{Outcome, Run};
use super::state::QueueState;
use super::values;
use crate::clock::millis;
use crate::sqlite::sql_error;
use crate::{Error, ErrorKind, Result};

/// How long a lease holds a job its worker runs; the loop extends it every
/// half lease, so that it bounds how long a vanished worker keeps a job, not
/// how long a handler may take.
pub(crate) const LEASE: Duration = Duration::from_secs(30);

/// How long a stopping worker waits for its handlers past their timeout
/// before it lets them go, and how long a failed write waits to try again.
const GRACE: Duration = Duration::from_secs(10);
const RETRY_WRITE: Duration = Duration::from_secs(1);

const SPILLED_VALUE: &str = "select value from _tinystore_jobs_spilled where id = ?1";

/// What handlers hand back to their loop, and what wakes it.
#[derive(Default)]
pub(crate) struct Inbox {
    state: Mutex<InboxState>,
    changed: Condvar,
}

#[derive(Default)]
struct InboxState {
    /// Each job a handler finished, and how to settle it; none for a job a
    /// cancel took, which settles nothing.
    finished: Vec<(Arc<Lease>, Option<How>)>,
    woken: bool,
    stopping: bool,
}

impl Inbox {
    fn finish(&self, lease: Arc<Lease>, how: Option<How>) {
        self.lock().finished.push((lease, how));
        self.changed.notify_all();
    }

    fn stop(&self) {
        self.lock().stopping = true;
        self.changed.notify_all();
    }

    fn stopping(&self) -> bool {
        self.lock().stopping
    }

    fn take(&self) -> Vec<(Arc<Lease>, Option<How>)> {
        std::mem::take(&mut self.lock().finished)
    }

    /// Sleeps until a handler finishes or `sleep` passes: a stopping loop's
    /// wait for its handlers, which the stop it already saw does not cut short.
    fn wait_finished(&self, sleep: Duration) {
        let state = self.lock();
        let _ = self
            .changed
            .wait_timeout_while(state, sleep, |state| state.finished.is_empty())
            .unwrap_or_else(PoisonError::into_inner);
    }

    /// Sleeps until a handler finishes, the alarm is lowered, the worker
    /// stops, or `sleep` passes.
    fn wait(&self, sleep: Duration) {
        let state = self.lock();
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, sleep, |state| state.finished.is_empty() && !state.woken && !state.stopping)
            .unwrap_or_else(PoisonError::into_inner);
        state.woken = false;
    }

    fn lock(&self) -> MutexGuard<'_, InboxState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Wake for Inbox {
    fn wake(&self) {
        self.lock().woken = true;
        self.changed.notify_all();
    }
}

/// Where a worker's loop hands the jobs it claims: its handler threads, or a
/// client across the wire. Either answers each job through the worker's
/// inbox.
pub(crate) trait Hand: Send + Sync {
    /// Takes a job, answered later through the inbox; or answers it at once,
    /// none for one a cancel took, which settles nothing.
    fn hand(&self, lease: Arc<Lease>) -> std::result::Result<(), Option<How>>;

    /// Takes nothing more, and answers the jobs it was handed and had not
    /// started, which go back uncounted.
    fn close(&self) -> Vec<Arc<Lease>>;
}

/// The claimed jobs on their way to the handlers, which read their values.
#[derive(Default)]
struct Feed {
    state: Mutex<(VecDeque<Arc<Lease>>, bool)>,
    ready: Condvar,
}

impl Feed {
    fn push(&self, lease: Arc<Lease>) {
        self.lock().0.push_back(lease);
        self.ready.notify_one();
    }

    /// The next job for a handler; none once the feed is closed and empty.
    fn take(&self) -> Option<Arc<Lease>> {
        let state = self.lock();
        let mut state = self
            .ready
            .wait_while(state, |(jobs, closed)| jobs.is_empty() && !*closed)
            .unwrap_or_else(PoisonError::into_inner);
        state.0.pop_front()
    }

    /// Closes the feed and answers the jobs no handler started.
    fn close(&self) -> Vec<Arc<Lease>> {
        let mut state = self.lock();
        state.1 = true;
        let unstarted = state.0.drain(..).collect();
        drop(state);
        self.ready.notify_all();
        unstarted
    }

    fn lock(&self) -> MutexGuard<'_, (VecDeque<Arc<Lease>>, bool)> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Hand for Feed {
    fn hand(&self, lease: Arc<Lease>) -> std::result::Result<(), Option<How>> {
        self.push(lease);
        Ok(())
    }

    fn close(&self) -> Vec<Arc<Lease>> {
        Feed::close(self)
    }
}

/// One worker's loop: the jobs it holds, which its handlers run, and the
/// settlements it has yet to write.
struct Loop {
    jobs: Arc<Jobs>,
    queue: Arc<QueueState>,
    /// Jobs it may hold: running, or claimed for a handler still busy.
    hold: usize,
    inbox: Arc<Inbox>,
    hand: Arc<dyn Hand>,
    holding: HashMap<i64, Arc<Lease>>,
    pending: Vec<(Arc<Lease>, How)>,
    /// Jobs handed to the handlers and not yet finished.
    in_hand: usize,
    /// Whether it returns once nothing is due and nothing is held: `run_due`.
    until_idle: bool,
    /// Jobs its handlers finished, for `run_due`'s answer.
    finished: usize,
}

/// What one write of the loop claimed, and when the queue next needs a claim
/// when it took fewer than it wanted.
struct ClaimResult {
    leases: Vec<Lease>,
    abandoned: Vec<i64>,
    full: bool,
    next: Option<i64>,
}

impl Loop {
    fn new(jobs: Arc<Jobs>, queue: Arc<QueueState>, local: usize, inbox: Arc<Inbox>, hand: Arc<dyn Hand>) -> Loop {
        let hold = if queue.policy.bounded() { local } else { local * 2 };
        Loop {
            jobs,
            queue,
            hold,
            inbox,
            hand,
            holding: HashMap::new(),
            pending: Vec::new(),
            in_hand: 0,
            until_idle: false,
            finished: 0,
        }
    }

    /// Claims and settles until the worker stops, the store closes, or, until
    /// idle, nothing more may start and nothing is held; then gives back what
    /// no handler started, waits for the handlers under way and writes what
    /// they answered.
    fn run(&mut self) -> Result<usize> {
        let looped = self.cycle();
        for lease in self.hand.close() {
            self.in_hand -= 1;
            self.pending.push((lease, How::GiveBack));
        }
        let drained = self.drain();
        looped.and(drained).map(|()| self.finished)
    }

    /// Waits for the handlers under way, each bounded by its timeout and a
    /// grace, writing what they answer as it comes and extending the leases
    /// they still run on, so that no other claim takes a job a handler runs.
    fn drain(&mut self) -> Result<()> {
        let deadline = Instant::now() + self.queue.policy.timeout + GRACE;
        loop {
            self.gather();
            let now = self.jobs.now();
            let left = deadline.saturating_duration_since(Instant::now());
            if self.in_hand == 0 || left.is_zero() {
                return self.write(now, 0).map(drop);
            }
            self.extend_due(now);
            let sleep = match self.write(now, 0) {
                Ok(_) => self.next_extension(now).map_or(LONGEST_SLEEP, |at| span(at - now)),
                Err(error) if error.kind() == ErrorKind::Closed => return Err(error),
                Err(error) => {
                    self.queue.writes.observe(&self.queue.name, &error.to_string());
                    RETRY_WRITE
                }
            };
            self.inbox.wait_finished(sleep.min(left).min(LONGEST_SLEEP));
        }
    }

    fn cycle(&mut self) -> Result<()> {
        loop {
            self.gather();
            if self.inbox.stopping() || self.jobs.closed() {
                return Ok(());
            }
            let now = self.jobs.now();
            self.extend_due(now);
            let (want, read, rated) = self.wanted(now);
            let result = self.write(now, want);
            self.answer(read, result.as_ref().ok());
            let result = match result {
                Ok(result) => result,
                Err(error) if self.until_idle || error.kind() == ErrorKind::Closed => return Err(error),
                Err(error) => {
                    self.queue.writes.observe(&self.queue.name, &error.to_string());
                    self.inbox.wait(RETRY_WRITE);
                    continue;
                }
            };
            let full = want > 0 && result.full;
            self.dispatch(result.leases);
            if self.until_idle && self.idle(now, rated) {
                return Ok(());
            }
            if !full {
                self.wait(now, rated);
            }
        }
    }

    /// How many jobs to claim at `now`: as many as handlers are free, when the
    /// alarm says a job may be due. `rated` is when the queue's rate next lets
    /// a job start, when it lets none start now.
    fn wanted(&self, now: i64) -> (usize, Option<Read>, Option<i64>) {
        let finishing = self.pending.iter().filter(|(_, how)| !matches!(how, How::Extend { .. })).count();
        let free = (self.hold + finishing).saturating_sub(self.holding.len());
        if free == 0 {
            return (0, None, None);
        }
        let room = match &self.queue.rate {
            Some(rate) => match rate.room(now) {
                (0, next) => return (0, None, next),
                (room, _) => room,
            },
            None => usize::MAX,
        };
        match self.queue.alarm.rung(now) {
            Some(read) => (free.min(CLAIM_BATCH).min(room), Some(read), None),
            None => (0, None, None),
        }
    }

    /// Ends the alarm read a claim made: a claim that found fewer jobs than it
    /// wanted knows when the queue is next due, a full or failed one does not.
    fn answer(&self, read: Option<Read>, result: Option<&ClaimResult>) {
        let Some(read) = read else {
            return;
        };
        match result {
            Some(result) if !result.full => self.queue.alarm.set(read, result.next),
            _ => self.queue.alarm.forget(read),
        }
    }

    /// Takes what the handlers finished, without waiting.
    fn gather(&mut self) {
        for (lease, how) in self.inbox.take() {
            self.in_hand -= 1;
            self.finished += 1;
            match how {
                Some(how) => self.pending.push((lease, how)),
                None => self.let_go(&lease),
            }
        }
    }

    /// Extends the leases held past half their length, so that a long handler
    /// keeps its job and a vanished process loses it within a lease.
    fn extend_due(&mut self, now: i64) {
        let half = millis(LEASE) / 2;
        for (id, lease) in &self.holding {
            let settling = self.pending.iter().any(|(pending, _)| pending.id == *id);
            if !settling && !lease.settled() && lease.until() - now <= half {
                self.pending.push((Arc::clone(lease), How::Extend { until: now + millis(LEASE) }));
            }
        }
    }

    /// Settles what is pending and claims up to `want` jobs in one grouped
    /// write. A settlement whose lease another claim has taken is dropped.
    fn write(&mut self, now: i64, want: usize) -> Result<ClaimResult> {
        if self.pending.is_empty() && want == 0 {
            return Ok(ClaimResult { leases: Vec::new(), abandoned: Vec::new(), full: false, next: None });
        }
        let pending = self.pending.clone();
        let queue = Arc::clone(&self.queue);
        let until = now + millis(LEASE);
        let (settled, result) = self.jobs.file().write(0, move |tx| {
            let mut settled = Vec::with_capacity(pending.len());
            for (lease, how) in &pending {
                settled.push(claim::settle(tx, &queue, lease, how, now)?);
            }
            if want == 0 {
                return Ok((
                    settled,
                    ClaimResult { leases: Vec::new(), abandoned: Vec::new(), full: false, next: None },
                ));
            }
            let claiming = Claiming { queue: &queue, now, until, limit: want };
            let Claimed { leases, abandoned, more } = claim::claim(tx, claiming)?;
            let full = leases.len() + abandoned.len() == want || more;
            let next = if full { None } else { claim::next_claim(tx, &claiming)? };
            Ok((settled, ClaimResult { leases, abandoned, full, next }))
        })?;
        self.settled(settled, now);
        for _ in &result.abandoned {
            self.queue.failures.observe(&self.queue.name, "its attempts ended without a settlement");
        }
        Ok(result)
    }

    /// Follows in memory what the written settlements changed.
    fn settled(&mut self, settled: Vec<Settled>, now: i64) {
        let mut room = false;
        for ((lease, how), settled) in std::mem::take(&mut self.pending).into_iter().zip(settled) {
            let extension = matches!(how, How::Extend { .. });
            lease.apply(&settled, extension);
            if let Some(due) = settled.due {
                self.queue.alarm.lower(due);
            }
            if let Some(failed) = &settled.failed {
                self.queue.failures.observe(&self.queue.name, failed);
            }
            if settled.lost && !lease.cancelled() {
                self.queue
                    .lost
                    .observe(&self.queue.name, "a lease ended while its handler ran, and another claim took the job");
            }
            if !extension || settled.lost {
                self.let_go(&lease);
                room = true;
            }
        }
        if room {
            self.queue.room_made(now);
        }
    }

    fn let_go(&mut self, lease: &Lease) {
        self.holding.remove(&lease.id);
        self.queue.release(lease);
    }

    /// Hands claimed jobs to the handlers, which read their values.
    fn dispatch(&mut self, leases: Vec<Lease>) {
        for lease in leases {
            let lease = Arc::new(lease);
            self.holding.insert(lease.id, Arc::clone(&lease));
            self.queue.hold(&lease);
            match self.hand.hand(Arc::clone(&lease)) {
                Ok(()) => self.in_hand += 1,
                Err(Some(how)) => self.pending.push((lease, how)),
                Err(None) => self.let_go(&lease),
            }
        }
    }

    /// Whether the loop holds nothing, has nothing to write, and no job is due,
    /// or the queue's rate lets none start until `rated`.
    fn idle(&self, now: i64, rated: Option<i64>) -> bool {
        self.holding.is_empty() && self.pending.is_empty() && (rated.is_some() || !self.queue.alarm.due(now))
    }

    /// Sleeps until a handler finishes, the alarm rings or is lowered, a held
    /// lease needs extending, or the worker stops. With every handler busy the
    /// alarm does not set the time, and with the rate holding it back only the
    /// time the rate names does.
    fn wait(&self, now: i64, rated: Option<i64>) {
        let mut sleep = match rated {
            Some(rated) => span(rated - now).min(LONGEST_SLEEP),
            None if self.holding.len() >= self.hold => LONGEST_SLEEP,
            None => self.queue.alarm.sleep(now),
        };
        if let Some(first) = self.next_extension(now) {
            sleep = sleep.min(span(first - now));
        }
        if !sleep.is_zero() {
            self.inbox.wait(sleep);
        }
    }

    /// When the first lease held is due to be extended: half a lease before it
    /// ends.
    fn next_extension(&self, now: i64) -> Option<i64> {
        let half = millis(LEASE) / 2;
        let first = self.holding.values().filter(|lease| !lease.settled()).map(|lease| lease.until() - half).min();
        first.map(|first| first.max(now))
    }
}

fn span(ms: i64) -> Duration {
    Duration::from_millis(u64::try_from(ms).unwrap_or(0))
}

/// Runs a handler on each job the feed hands over, until it closes.
fn serve<V, F, R, E>(jobs: &Arc<Jobs>, queue: &Arc<QueueState>, feed: &Feed, inbox: &Inbox, handler: &F)
where
    V: DeserializeOwned,
    F: Fn(V, &Run) -> std::result::Result<R, E>,
    R: Into<Outcome>,
    E: fmt::Display,
{
    while let Some(lease) = feed.take() {
        let how = run_one(jobs, queue, &lease, handler);
        inbox.finish(lease, how);
    }
}

/// Reads a job's value, runs the handler on it and says how to settle it;
/// none when a cancel took the job, which then settles nothing.
fn run_one<V, F, R, E>(jobs: &Arc<Jobs>, queue: &Arc<QueueState>, lease: &Arc<Lease>, handler: &F) -> Option<How>
where
    V: DeserializeOwned,
    F: Fn(V, &Run) -> std::result::Result<R, E>,
    R: Into<Outcome>,
    E: fmt::Display,
{
    if lease.cancelled() {
        return None;
    }
    let value = match read_value::<V>(jobs, lease) {
        Ok(value) => value,
        Err(error) if error.kind() == ErrorKind::Invalid => return Some(How::Fail { cause: error.to_string() }),
        Err(error) => return Some(How::Retry { at: None, cause: error.to_string() }),
    };
    if !lease.begin(jobs.now()) {
        return None;
    }
    let run = Run::new(Arc::clone(lease), Arc::clone(queue), Arc::clone(jobs));
    let answered = catch_unwind(AssertUnwindSafe(|| handler(value, &run)));
    if lease.cancelled() {
        return None;
    }
    Some(match answered {
        Ok(Ok(outcome)) => outcome.into().how(jobs.now()),
        Ok(Err(error)) => How::Retry { at: None, cause: error.to_string() },
        Err(panic) => {
            let message = panic_message(&*panic);
            queue.panics.observe(&queue.name, &message);
            How::Retry { at: None, cause: format!("the handler panicked: {message}") }
        }
    })
}

/// A claimed job's value, read from its own row outside the writer when it
/// spilled; one that no longer reads as the queue's type is `invalid`.
pub(crate) fn read_value<V: DeserializeOwned>(jobs: &Jobs, lease: &Lease) -> Result<V> {
    let encoded = match (&lease.inline, lease.spill) {
        (_, Some(spill)) => jobs.file().read(|c| {
            c.prepare_cached(SPILLED_VALUE)
                .and_then(|mut select| select.query_row([spill], |row| row.get::<_, Vec<u8>>(0)))
                .map_err(|error| sql_error("the value a claimed job spilled", error))
        })?,
        (Some(inline), None) => inline.clone(),
        (None, None) => Vec::new(),
    };
    values::decode(&encoded)
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic without a message".to_owned())
}

/// A worker running a queue's handlers on threads the store started for it.
/// Dropped, it keeps running until the store closes; `stop` stops it.
#[must_use = "a worker runs until it is stopped or the store closes; keep it to stop it"]
pub struct Worker {
    running: Arc<Running>,
}

/// What a worker's threads share with the handle and the engine that stop it.
pub(crate) struct Running {
    inbox: Arc<Inbox>,
    hand: Arc<dyn Hand>,
    looping: Mutex<Option<JoinHandle<()>>>,
    handlers: Mutex<Vec<JoinHandle<()>>>,
    alive: Alive,
    stopped: AtomicBool,
}

/// Handler threads that have not yet ended, and the signal each gives as it
/// ends.
type Alive = Arc<(Mutex<usize>, Condvar)>;

impl Running {
    pub(crate) fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Takes no job more, waits for the handlers under way, each bounded by
    /// its timeout, and writes what they answered. A handler that never
    /// returns is let go after its timeout and a grace, its job given back
    /// when its lease ends.
    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.inbox.stop();
        let looping = self.looping.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(looping) = looping.filter(|handle| handle.thread().id() != thread::current().id()) {
            let _ = looping.join();
        }
        self.hand.close();
        let (alive, ended) = &*self.alive;
        let alive = alive.lock().unwrap_or_else(PoisonError::into_inner);
        let (alive, _) =
            ended.wait_timeout_while(alive, GRACE, |alive| *alive > 0).unwrap_or_else(PoisonError::into_inner);
        if *alive == 0 {
            drop(alive);
            for handle in self.handlers.lock().unwrap_or_else(PoisonError::into_inner).drain(..) {
                if handle.thread().id() != thread::current().id() {
                    let _ = handle.join();
                }
            }
        }
    }
}

impl Worker {
    /// Takes no job more and waits for the handlers under way, each bounded by
    /// its queue's timeout; the store's close stops every worker so.
    pub fn stop(&self) {
        self.running.stop();
    }
}

impl fmt::Debug for Worker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Worker")
    }
}

/// Starts a worker of `local` handlers on threads of the store's.
pub(crate) fn start<V, F, R, E>(jobs: &Arc<Jobs>, queue: &Arc<QueueState>, local: usize, handler: F) -> Result<Worker>
where
    V: DeserializeOwned + 'static,
    F: Fn(V, &Run) -> std::result::Result<R, E> + Send + Sync + 'static,
    R: Into<Outcome>,
    E: fmt::Display,
{
    if jobs.closed() {
        return Err(Error::closed(queue.describe()));
    }
    let (inbox, feed) = (Arc::new(Inbox::default()), Arc::new(Feed::default()));
    queue.alarm.subscribe(Arc::downgrade(&inbox) as Weak<dyn Wake>);
    let handler = Arc::new(handler);
    let alive = Arc::new((Mutex::new(0usize), Condvar::new()));
    let mut handlers = Vec::with_capacity(local);
    for at in 0..local {
        *alive.0.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        let (jobs, queue_of, fed, inbox, handler, count) = (
            Arc::clone(jobs),
            Arc::clone(queue),
            Arc::clone(&feed),
            Arc::clone(&inbox),
            Arc::clone(&handler),
            Arc::clone(&alive),
        );
        let name = format!("tinystore-jobs-{}-{at}", queue.name);
        let spawned = thread::Builder::new().name(name).spawn(move || {
            serve(&jobs, &queue_of, &fed, &inbox, &*handler);
            let (alive, ended) = &*count;
            *alive.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
            ended.notify_all();
        });
        match spawned {
            Ok(handle) => handlers.push(handle),
            Err(error) => {
                *alive.0.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
                feed.close();
                return Err(Error::io(format!("{}: a handler's thread", queue.describe()), error));
            }
        }
    }
    let hand: Arc<dyn Hand> = feed;
    let looped = Loop::new(Arc::clone(jobs), Arc::clone(queue), local, inbox, hand);
    let running = run_loop(looped, (handlers, alive), None)?;
    Ok(Worker { running })
}

/// Where the answers for the jobs a client's worker holds go: its loop's
/// inbox. The client answers each job it was sent, and a cancel answers one
/// it took, so that each is answered once.
#[derive(Clone, Default)]
pub(crate) struct Answers(Arc<Inbox>);

impl Answers {
    /// How the job's run ended; none for one a cancel took, which settles
    /// nothing.
    pub(crate) fn answer(&self, lease: Arc<Lease>, how: Option<How>) {
        self.0.finish(lease, how);
    }
}

/// A worker whose handlers are a client's: its loop runs on a thread of the
/// store's and hands its jobs to `hand`, and their answers come back through
/// its [`Answers`].
pub(crate) struct Remote {
    running: Arc<Running>,
}

impl Remote {
    /// Takes no job more and returns: the loop gives back the jobs it holds
    /// and has not handed over, and writes the answers that still come.
    pub(crate) fn halt(&self) {
        self.running.inbox.stop();
    }

    /// Halts the worker and waits for its loop to write what was answered; a
    /// job handed and not yet answered holds the stop up to the queue's
    /// timeout and a grace.
    pub(crate) fn stop(&self) {
        self.running.stop();
    }
}

impl fmt::Debug for Remote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Remote")
    }
}

/// What a client's worker asks of its loop.
pub(crate) struct RemoteWork {
    /// The client's handlers.
    pub(crate) local: usize,
    /// Whether the loop ends once nothing is due and nothing is held: `runDue`.
    pub(crate) until_idle: bool,
    pub(crate) answers: Answers,
    pub(crate) hand: Arc<dyn Hand>,
    pub(crate) ended: Ended,
}

/// Hears, on the loop's thread, how a loop ended: the jobs its handlers
/// finished, or why it stopped.
pub(crate) type Ended = Box<dyn FnOnce(Result<usize>) + Send>;

pub(crate) fn start_remote(jobs: &Arc<Jobs>, queue: &Arc<QueueState>, work: RemoteWork) -> Result<Remote> {
    if jobs.closed() {
        return Err(Error::closed(queue.describe()));
    }
    let RemoteWork { local, until_idle, answers, hand, ended } = work;
    let inbox = Arc::clone(&answers.0);
    queue.alarm.subscribe(Arc::downgrade(&inbox) as Weak<dyn Wake>);
    let mut looped = Loop::new(Arc::clone(jobs), Arc::clone(queue), local, inbox, hand);
    looped.until_idle = until_idle;
    let running = run_loop(looped, (Vec::new(), Alive::default()), Some(ended))?;
    Ok(Remote { running })
}

/// Starts a worker's loop on a thread of the store's, which the engine stops
/// at close; `ended` hears how the loop ended.
fn run_loop(
    mut looped: Loop,
    (handlers, alive): (Vec<JoinHandle<()>>, Alive),
    ended: Option<Ended>,
) -> Result<Arc<Running>> {
    let (jobs, queue) = (Arc::clone(&looped.jobs), Arc::clone(&looped.queue));
    let (inbox, hand) = (Arc::clone(&looped.inbox), Arc::clone(&looped.hand));
    let describe = queue.describe();
    let looping = thread::Builder::new()
        .name(format!("tinystore-jobs-{}", queue.name))
        .spawn(move || {
            let ran = looped.run();
            if let Err(error) = &ran {
                tracing::warn!(target: "tinystore", queue = %describe, %error, "a worker stopped on an error");
            }
            if let Some(ended) = ended {
                ended(ran);
            }
        })
        .map_err(|error| {
            hand.close();
            Error::io(format!("{}: a worker's thread", queue.describe()), error)
        })?;
    let running = Arc::new(Running {
        inbox,
        hand,
        looping: Mutex::new(Some(looping)),
        handlers: Mutex::new(handlers),
        alive,
        stopped: AtomicBool::new(false),
    });
    jobs.keep_worker(&running);
    Ok(running)
}

/// Runs what is due when it starts and what falls due meanwhile on `local`
/// handlers of this call's own, then returns how many jobs ran.
pub(crate) fn run_due<V, F, R, E>(jobs: &Arc<Jobs>, queue: &Arc<QueueState>, local: usize, handler: &F) -> Result<usize>
where
    V: DeserializeOwned,
    F: Fn(V, &Run) -> std::result::Result<R, E> + Sync,
    R: Into<Outcome>,
    E: fmt::Display,
{
    let (inbox, feed) = (Arc::new(Inbox::default()), Arc::new(Feed::default()));
    queue.alarm.subscribe(Arc::downgrade(&inbox) as Weak<dyn Wake>);
    thread::scope(|scope| {
        for _ in 0..local {
            scope.spawn(|| serve(jobs, queue, &feed, &inbox, handler));
        }
        let hand: Arc<dyn Hand> = Arc::clone(&feed) as Arc<dyn Hand>;
        let mut looped = Loop::new(Arc::clone(jobs), Arc::clone(queue), local, Arc::clone(&inbox), hand);
        looped.until_idle = true;
        let ran = looped.run();
        feed.close();
        ran
    })
}
