//! A worker as a future: the claim loop on a thread of the store's, as a
//! client's worker over the wire has it, and the handlers polled inside the
//! future the program spawns.
//!
//! The loop hands each job it claims to the worker's [`Handing`], which
//! reads its value on the loop's thread and wakes the worker. The worker's
//! poll begins as many as its handlers may run, polls each handler whose own
//! waker woke, and answers the loop as each ends:
//!
//! ```text
//! loop's thread        claim → hand: value read, job ready → … → write what was answered
//! worker's poll                     begin → poll handler → … → answer
//! ```
//!
//! Every job the loop hands over is answered to it once: by its handler, by
//! a cancel that took it before it began, by the worker's drop, or by the
//! loop's own close, which gives back the jobs no handler began.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Wake, Waker};

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::queue::Queue;
use super::run::Run;
use crate::jobs::{self, Answers, Ended, Hand, How, Lease, Outcome, Remote, RemoteWork};
use crate::sync::lock;
use crate::{Error, Result};

/// What a job's handler said of it, a panic aside: its outcome, or the text
/// of its error, which retries it.
type Answering = Pin<Box<dyn Future<Output = std::result::Result<Outcome, String>> + Send>>;

/// The handler called on a job's value, once the worker begins it.
type Begin = Box<dyn FnOnce(Run) -> Answering + Send>;

/// Reads a leased job's value, on the loop's thread, and gives what begins
/// it; a value that cannot be read says how to settle its job instead.
type Make = Arc<dyn Fn(&Arc<Lease>) -> std::result::Result<Begin, How> + Send + Sync>;

/// A signal that stops a worker gracefully once it is ready.
type Signal = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The cause a job keeps when its worker's future was dropped while its
/// handler ran: it runs again after its backoff, the attempt counted.
const DROPPED: &str = "the worker's future was dropped while the job ran";

/// Jobs a worker begins in one poll at most before it yields its executor's
/// thread to other tasks, waking itself to begin the rest.
const BEGUN_IN_A_POLL: usize = 64;

impl<V> Queue<V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    /// A worker that runs `handler` on each job as it falls due, as many at
    /// once as the queue's concurrency lets, one when it sets none: a future
    /// the program spawns, which starts nothing before its first poll and
    /// runs until [`Worker::until`]'s signal, its drop or the store's close;
    /// the page of `tinystore::aio::jobs` says what each does.
    ///
    /// The handler gets the job's value and its run by value. `Ok(())` is
    /// done; an answer of the run's says otherwise; an error retries the job
    /// after its backoff, its text kept as the job's error, and so does a
    /// panic.
    pub fn work<F, W, R, E>(&self, handler: F) -> Worker
    where
        F: Fn(V, Run) -> W + Send + Sync + 'static,
        W: Future<Output = std::result::Result<R, E>> + Send + 'static,
        R: Into<Outcome> + 'static,
        E: fmt::Display + 'static,
    {
        self.workers(self.queue.state.policy.local()).work(handler)
    }

    /// Runs what is due when its future is first polled and what falls due
    /// meanwhile, then answers how many jobs ran; it never waits for a later
    /// job. Tests, scripts and commands use it. Its future is a worker's, and
    /// dropped it stops as a worker's drop does.
    pub fn run_due<F, W, R, E>(&self, handler: F) -> impl Future<Output = Result<usize>> + Send + 'static
    where
        F: Fn(V, Run) -> W + Send + Sync + 'static,
        W: Future<Output = std::result::Result<R, E>> + Send + 'static,
        R: Into<Outcome> + 'static,
        E: fmt::Display + 'static,
    {
        self.workers(self.queue.state.policy.local()).run_due(handler)
    }

    /// A worker that runs at most `handlers` at once, under the queue's
    /// concurrency.
    pub fn concurrency(&self, handlers: u32) -> Workers<'_, V> {
        self.workers(handlers)
    }

    fn workers(&self, handlers: u32) -> Workers<'_, V> {
        Workers { queue: self, handlers: usize::try_from(handlers).unwrap_or(1).max(1) }
    }
}

/// A worker being made: how many handlers it runs at once.
#[must_use = "a worker is made with work() or runs with run_due()"]
pub struct Workers<'a, V> {
    queue: &'a Queue<V>,
    handlers: usize,
}

impl<V> Workers<'_, V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    /// The worker; see [`Queue::work`].
    pub fn work<F, W, R, E>(self, handler: F) -> Worker
    where
        F: Fn(V, Run) -> W + Send + Sync + 'static,
        W: Future<Output = std::result::Result<R, E>> + Send + 'static,
        R: Into<Outcome> + 'static,
        E: fmt::Display + 'static,
    {
        Worker { driver: Driver::new(self.queue, self.handlers, false, handler), signals: Vec::new() }
    }

    /// Runs what is due; see [`Queue::run_due`].
    pub fn run_due<F, W, R, E>(self, handler: F) -> impl Future<Output = Result<usize>> + Send + 'static
    where
        F: Fn(V, Run) -> W + Send + Sync + 'static,
        W: Future<Output = std::result::Result<R, E>> + Send + 'static,
        R: Into<Outcome> + 'static,
        E: fmt::Display + 'static,
    {
        Driver::new(self.queue, self.handlers, true, handler)
    }
}

impl<V> fmt::Debug for Workers<'_, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Workers").field("handlers", &self.handlers).finish()
    }
}

/// A queue's worker: a future that runs its handlers, `Ok(())` once it
/// stopped. It starts at its first poll; spawn it on the program's runtime.
#[must_use = "a worker is a future: it runs only once spawned or awaited"]
pub struct Worker {
    driver: Driver,
    signals: Vec<Signal>,
}

impl Worker {
    /// Stops the worker gracefully once `signal` is ready, as
    /// `tokio::signal::ctrl_c()` is on a ctrl-c: it takes no job more, gives
    /// back uncounted the jobs it claimed and had not begun, waits for the
    /// handlers under way, each within the queue's timeout and a grace, and
    /// returns once their answers are written. Given several, the first that
    /// is ready stops it.
    pub fn until(mut self, signal: impl Future + Send + 'static) -> Worker {
        self.signals.push(Box::pin(async move {
            signal.await;
        }));
        self
    }

    /// A worker of `queue` that runs nothing and ends with `refused` at its
    /// first poll.
    pub(super) fn refused(queue: &Queue<()>, refused: Error) -> Worker {
        Worker { driver: Driver::of(queue, 1, Phase::Refused(refused)), signals: Vec::new() }
    }
}

impl Future for Worker {
    type Output = Result<()>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<()>> {
        let worker = self.get_mut();
        if worker.signals.iter_mut().any(|signal| signal.as_mut().poll(context).is_ready()) {
            worker.signals.clear();
            worker.driver.halt();
        }
        Pin::new(&mut worker.driver).poll(context).map(|ran| ran.map(drop))
    }
}

impl fmt::Debug for Worker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Worker({:?})", self.driver.queue)
    }
}

/// Where a worker is in its life.
enum Phase {
    /// Not yet polled: what its first poll starts the loop with.
    Unstarted {
        make: Make,
        until_idle: bool,
    },
    Running,
    /// Stopped before its first poll: it answers that no job ran.
    Halted,
    Refused(Error),
    Ended,
}

/// What a worker's future drives: the loop it started, and the handlers it
/// runs, a job each, by the number of their slot.
struct Driver {
    phase: Phase,
    shared: Arc<Shared>,
    answers: Answers,
    /// The queue, its values unread: the loop's thread reads them.
    queue: Queue<()>,
    /// Handlers it runs at once at most.
    local: usize,
    running: HashMap<u64, Handling>,
    last_slot: u64,
}

/// A handler under way: its job, and its future, polled when its own waker
/// woke.
struct Handling {
    lease: Arc<Lease>,
    answering: Answering,
    waker: Waker,
}

impl Driver {
    fn new<V, F, W, R, E>(queue: &Queue<V>, local: usize, until_idle: bool, handler: F) -> Driver
    where
        V: Serialize + DeserializeOwned + Send + 'static,
        F: Fn(V, Run) -> W + Send + Sync + 'static,
        W: Future<Output = std::result::Result<R, E>> + Send + 'static,
        R: Into<Outcome> + 'static,
        E: fmt::Display + 'static,
    {
        Driver::of(queue, local, Phase::Unstarted { make: maker(queue, handler), until_idle })
    }

    fn of<V>(queue: &Queue<V>, local: usize, phase: Phase) -> Driver
    where
        V: Serialize + DeserializeOwned + Send + 'static,
    {
        Driver {
            phase,
            shared: Arc::default(),
            answers: Answers::default(),
            queue: Queue { queue: queue.queue.retyped(), pool: Arc::clone(&queue.pool) },
            local,
            running: HashMap::new(),
            last_slot: 0,
        }
    }

    /// Starts the loop on a thread of the store's, handing its jobs here.
    fn start(&mut self, make: Make, until_idle: bool) -> Result<()> {
        let hand: Arc<dyn Hand> = Arc::new(Handing { shared: Arc::downgrade(&self.shared), make });
        let ended = loop_ended(Arc::clone(&self.shared));
        let work = RemoteWork { local: self.local, until_idle, answers: self.answers.clone(), hand, ended };
        let blocking = &self.queue.queue;
        let remote =
            jobs::start_remote(&blocking.jobs, &blocking.state, work).map_err(|error| blocking.fail(None, error))?;
        let mut shared = self.shared.lock();
        if shared.ended.is_some() {
            drop(shared);
            remote.ended();
        } else {
            shared.remote = Some(remote);
        }
        Ok(())
    }

    /// Polls the handlers whose wakers woke, begins the jobs handed over while
    /// a handler is free, and ends once the loop did.
    fn drive(&mut self, context: &mut Context<'_>) -> Poll<Result<usize>> {
        let (woken, ended) = {
            let mut shared = self.shared.lock();
            shared.waker = Some(context.waker().clone());
            (std::mem::take(&mut shared.woken), shared.ended.take())
        };
        if let Some(ran) = ended {
            self.phase = Phase::Ended;
            // what is left is what the loop let go after the queue's timeout and a grace
            self.running.clear();
            return Poll::Ready(ran);
        }
        for slot in woken {
            self.poll_handler(slot);
        }
        for _ in 0..BEGUN_IN_A_POLL {
            let Some((lease, begin)) = self.next_ready() else {
                return Poll::Pending;
            };
            self.begin(lease, begin);
        }
        // handlers that end at once, fed by a loop that claims as fast, would hold the executor's thread
        context.waker().wake_by_ref();
        Poll::Pending
    }

    /// The next job handed over, while a handler is free and the worker
    /// takes jobs; a halted worker's are the loop's to give back.
    fn next_ready(&self) -> Option<(Arc<Lease>, Begin)> {
        if self.running.len() >= self.local {
            return None;
        }
        let mut shared = self.shared.lock();
        if shared.halted { None } else { shared.ready.pop_front() }
    }

    /// Begins a job and polls its handler a first time; one a cancel took
    /// first is answered as it is, settling nothing.
    fn begin(&mut self, lease: Arc<Lease>, begin: Begin) {
        let run = Run::of(&self.queue, Arc::clone(&lease));
        if !run.begin() {
            return self.answers.answer(lease, None);
        }
        let answering = match catch_unwind(AssertUnwindSafe(|| begin(run))) {
            Ok(answering) => answering,
            Err(panic) => return self.answer(lease, Err(panic)),
        };
        self.last_slot += 1;
        let slot = self.last_slot;
        let waker = Waker::from(Arc::new(Slot { number: slot, shared: Arc::downgrade(&self.shared) }));
        self.running.insert(slot, Handling { lease, answering, waker });
        self.poll_handler(slot);
    }

    /// Polls one handler with its own waker, and answers its job once it ends,
    /// a panic among its ends.
    fn poll_handler(&mut self, slot: u64) {
        let Some(handling) = self.running.get_mut(&slot) else {
            return;
        };
        let mut context = Context::from_waker(&handling.waker);
        let ran = match catch_unwind(AssertUnwindSafe(|| handling.answering.as_mut().poll(&mut context))) {
            Ok(Poll::Pending) => return,
            Ok(Poll::Ready(answer)) => Ok(answer),
            Err(panic) => Err(panic),
        };
        if let Some(Handling { lease, answering, .. }) = self.running.remove(&slot) {
            drop(answering);
            self.answer(lease, ran);
        }
    }

    fn answer(&self, lease: Arc<Lease>, ran: std::thread::Result<std::result::Result<Outcome, String>>) {
        let blocking = &self.queue.queue;
        let how = jobs::answered(&blocking.jobs, &blocking.state, &lease, ran);
        self.answers.answer(lease, how);
    }

    /// Takes no job more: before the first poll it starts nothing, and after
    /// it the loop gives back what no handler began and writes what the
    /// handlers under way answer.
    fn halt(&mut self) {
        match self.phase {
            Phase::Unstarted { .. } => self.phase = Phase::Halted,
            Phase::Running => self.shared.halt(),
            Phase::Halted | Phase::Refused(_) | Phase::Ended => {}
        }
    }
}

impl Future for Driver {
    type Output = Result<usize>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<usize>> {
        let driver = self.get_mut();
        match std::mem::replace(&mut driver.phase, Phase::Running) {
            Phase::Running => {}
            Phase::Unstarted { make, until_idle } => {
                if let Err(error) = driver.start(make, until_idle) {
                    driver.phase = Phase::Ended;
                    return Poll::Ready(Err(error));
                }
            }
            Phase::Halted => {
                driver.phase = Phase::Ended;
                return Poll::Ready(Ok(0));
            }
            Phase::Refused(error) => {
                driver.phase = Phase::Ended;
                return Poll::Ready(Err(error));
            }
            Phase::Ended => {
                driver.phase = Phase::Ended;
                return Poll::Ready(Err(Error::internal(format!(
                    "{:?}: a worker polled after it ended",
                    driver.queue
                ))));
            }
        }
        driver.drive(context)
    }
}

impl Drop for Driver {
    /// The abort: the loop takes no job more and gives back those no handler
    /// began, and each job whose handler's future goes with this one runs
    /// again after its backoff, the attempt counted. It waits for nothing:
    /// the loop writes it and ends on its own thread.
    fn drop(&mut self) {
        if !matches!(self.phase, Phase::Running) {
            return;
        }
        self.shared.halt();
        for (_, Handling { lease, answering, .. }) in self.running.drain() {
            drop(answering);
            let how = (!lease.cancelled()).then(|| How::Retry { at: None, cause: DROPPED.to_owned() });
            self.answers.answer(lease, how);
        }
    }
}

/// What the worker's future and its loop share: the jobs handed over and not
/// begun, the handlers woken, the task to wake, and the loop while it runs.
#[derive(Default)]
struct Shared {
    state: Mutex<SharedState>,
}

#[derive(Default)]
struct SharedState {
    /// The jobs handed over and not yet begun, in the order handed.
    ready: VecDeque<(Arc<Lease>, Begin)>,
    /// The slots whose handler's waker woke since the worker's last poll.
    woken: Vec<u64>,
    /// The waker of the task that polls the worker.
    waker: Option<Waker>,
    /// It begins no job more, and the loop gives back the ones handed.
    halted: bool,
    /// How the loop ended, once it did, until the worker's poll takes it.
    ended: Option<Result<usize>>,
    /// The loop, while it runs.
    remote: Option<Remote>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, SharedState> {
        lock(&self.state)
    }

    /// Begins no job more, and has the loop take no job more.
    fn halt(&self) {
        let mut state = self.lock();
        state.halted = true;
        if let Some(remote) = &state.remote {
            remote.halt();
        }
    }

    /// Wakes the task that polls the worker, outside the lock.
    fn wake(&self, change: impl FnOnce(&mut SharedState)) {
        let waker = {
            let mut state = self.lock();
            change(&mut state);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// What hears the loop end, on its thread: the worker's next poll returns
/// how it ended, and the engine forgets the loop.
fn loop_ended(shared: Arc<Shared>) -> Ended {
    Box::new(move |ran| {
        let mut remote = None;
        shared.wake(|state| {
            state.ended = Some(ran);
            remote = state.remote.take();
        });
        if let Some(remote) = remote {
            remote.ended();
        }
    })
}

/// The worker's side of its loop: each job the loop claims is handed here,
/// its value read on the loop's thread, and waits until a handler is free.
/// It holds the worker's state weakly, since that state holds the loop.
struct Handing {
    shared: Weak<Shared>,
    make: Make,
}

impl Hand for Handing {
    fn hand(&self, lease: Arc<Lease>) -> std::result::Result<(), Option<How>> {
        if lease.cancelled() {
            return Err(None);
        }
        let Some(shared) = self.shared.upgrade().filter(|shared| !shared.lock().halted) else {
            return Err(Some(How::GiveBack));
        };
        let begin = (self.make)(&lease).map_err(Some)?;
        let mut handed = Some((lease, begin));
        shared.wake(|state| {
            if !state.halted {
                state.ready.extend(handed.take());
            }
        });
        match handed {
            None => Ok(()),
            Some(_) => Err(Some(How::GiveBack)),
        }
    }

    fn close(&self) -> Vec<Arc<Lease>> {
        let Some(shared) = self.shared.upgrade() else {
            return Vec::new();
        };
        let ready = {
            let mut state = shared.lock();
            state.halted = true;
            std::mem::take(&mut state.ready)
        };
        ready.into_iter().map(|(lease, _)| lease).collect()
    }
}

/// One handler's waker: it marks its slot woken and wakes the worker's task,
/// so that the worker polls that handler alone.
struct Slot {
    number: u64,
    shared: Weak<Shared>,
}

impl Wake for Slot {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(shared) = self.shared.upgrade() {
            shared.wake(|state| {
                if !state.woken.contains(&self.number) {
                    state.woken.push(self.number);
                }
            });
        }
    }
}

/// What reads a job's value on the loop's thread and gives what begins it:
/// `handler` called on the value with the job's run, its answer erased of
/// its types.
fn maker<V, F, W, R, E>(queue: &Queue<V>, handler: F) -> Make
where
    V: Serialize + DeserializeOwned + Send + 'static,
    F: Fn(V, Run) -> W + Send + Sync + 'static,
    W: Future<Output = std::result::Result<R, E>> + Send + 'static,
    R: Into<Outcome> + 'static,
    E: fmt::Display + 'static,
{
    let (engine, handler) = (Arc::clone(&queue.queue.jobs), Arc::new(handler));
    Arc::new(move |lease: &Arc<Lease>| {
        let value = jobs::value_or_how::<V>(&engine, lease)?;
        let handler = Arc::clone(&handler);
        let begin: Begin = Box::new(move |run: Run| -> Answering {
            let answering = handler(value, run);
            Box::pin(async move { answering.await.map(Into::into).map_err(|error| error.to_string()) })
        });
        Ok(begin)
    })
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
