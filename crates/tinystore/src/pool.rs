//! The store's pool: its threads for the calls that must not run where they
//! were made. A connection's calls run here, off the thread that reads its
//! frames, and so do an awaited call's reads, commits and transactions, off
//! the executor that polls it.
//!
//! The pool is the store's, one for the wire and the async API both, so that
//! one bound holds the threads of both and a database's transactions wait in
//! one line whoever asked for them.

use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::Result;
use crate::Store;
use crate::engine::{Engine, Host};

/// Threads the pool runs at most: a write blocks its thread until its group
/// commits, and the threads waiting together are the writes one commit can
/// carry. A job that must run, a commit's lead, finds as many again beside
/// them when these are all busy (`Pool::run_always`).
pub(crate) const MOST: usize = 16;
/// How long a thread that found no job looks for the next before it sleeps.
/// Waking a thread that sleeps costs more than a point read, and under a
/// hypervisor ten times more: a read through the pool took 52 microseconds
/// where its thread slept between two, and 10 where it had not.
const LINGER: Duration = Duration::from_micros(200);
/// How long a thread sleeps without a job before it ends, as a file's idle
/// readers close after a minute: a burst of calls otherwise keeps sixteen
/// threads and their stacks for as long as the store is open.
const RETIRE_AFTER: Duration = Duration::from_secs(60);
/// The name of every thread of the pool.
pub(crate) const THREAD: &str = "tinystore-pool";
/// The name of the thread a job that must run starts of its own, while every
/// thread of the pool is busy or once the pool stopped.
#[cfg(feature = "async")]
pub(crate) const ALONE: &str = "tinystore-alone";

pub(crate) type Job = Box<dyn FnOnce() + Send>;

/// The store's threads: started as calls need them, each ended once it slept
/// a minute without a job, and all of them stopped when the store closes,
/// which waits for the calls under way.
pub(crate) struct Pool {
    queue: Mutex<Queue>,
    ready: Condvar,
    /// Jobs waiting, which the thread that lingers reads without the lock.
    waiting: AtomicUsize,
    closing: AtomicBool,
    linger: Duration,
    retire_after: Duration,
    threads: Mutex<Vec<JoinHandle<()>>>,
    /// The jobs waiting on each line, by its name, while one of its jobs
    /// runs; a line that runs none has no entry.
    lines: Mutex<HashMap<String, VecDeque<Job>>>,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    idle: usize,
    /// Whether a thread was woken and has not taken its job yet. One wake is
    /// under way at a time: whoever queues a job wakes one thread, and that
    /// thread wakes the next when jobs still wait, or starts it when none
    /// sleeps. A wake a job cost the thread that reads a connection more
    /// than the job itself, and with 64 calls in flight that thread is all
    /// the connection has.
    waking: bool,
    /// Whether a thread that found no job is looking for the next before it
    /// sleeps. One does at a time, and a job that comes meanwhile is its own:
    /// it wakes nobody and starts no thread.
    lingering: bool,
    /// When a thread ran a job and found none after it, until the next job
    /// is taken.
    emptied: Option<Instant>,
    /// Whether the job that last found the queue empty was taken within the
    /// linger of its emptying. Only then does a thread linger: a thread
    /// looking for jobs that come further apart would keep a processor busy
    /// for nothing.
    dense: bool,
    started: usize,
    /// Threads started that have not looked at the queue yet: each takes a
    /// job waiting, so a job they will take starts no thread more.
    coming: usize,
    stopped: bool,
    /// Jobs that must run, given while every thread was busy, which the
    /// threads started for them take in order: never a thread of the pool's
    /// most, which may all wait for what one of these frees.
    #[cfg(feature = "async")]
    musts: VecDeque<Job>,
    /// The threads started for them, at most [`MOST`].
    #[cfg(feature = "async")]
    alone: usize,
}

impl Pool {
    /// The store's pool, which starts no thread before its first job.
    pub(crate) fn of(store: &Store) -> Result<Arc<Pool>> {
        store.engine(|_| Ok(Arc::new(Pool::new(LINGER, RETIRE_AFTER))))
    }

    /// The store's pool, or for a store that closed one stopped already,
    /// which drops every job unrun: what an awaited call on a closed store
    /// answers `Closed` through, where a handle is made without a `Result`.
    #[cfg(feature = "async")]
    pub(crate) fn of_or_stopped(store: &Store) -> Arc<Pool> {
        Pool::of(store).unwrap_or_else(|_| Pool::stopped())
    }

    /// A pool stopped already, which drops every job unrun: a handle's whose
    /// store is gone.
    #[cfg(feature = "async")]
    pub(crate) fn stopped() -> Arc<Pool> {
        let stopped = Pool::new(LINGER, RETIRE_AFTER);
        stopped.lock().stopped = true;
        Arc::new(stopped)
    }

    fn new(linger: Duration, retire_after: Duration) -> Pool {
        Pool {
            queue: Mutex::new(Queue::default()),
            ready: Condvar::new(),
            waiting: AtomicUsize::new(0),
            closing: AtomicBool::new(false),
            linger,
            retire_after,
            threads: Mutex::new(Vec::new()),
            lines: Mutex::new(HashMap::new()),
        }
    }

    /// Runs `job` on the pool once the jobs queued on `line` before it have
    /// ended: a line runs one job at a time, in their order, on one thread,
    /// and the jobs waiting on it hold none.
    ///
    /// A database's transactions are a line. Each on a thread of its own
    /// waited there for the file's writer: sixteen took every thread, and the
    /// call the client of the one holding the writer made next, around the
    /// transaction, waited behind them until its bound rolled it back. The
    /// waiters also took the writer in no order: with slow commits, one
    /// waited past its five seconds while those after it committed.
    ///
    /// A pool that stopped drops `job` unrun, as [`Pool::run`] does: a line
    /// it began would never end, and the jobs queued behind its first would
    /// wait for ever.
    #[allow(dead_code, reason = "only transactions wait in a line: sql's over the wire, and the async API's")]
    pub(crate) fn run_in_line(self: &Arc<Self>, line: &str, job: Job) {
        let mut lines = self.lock_lines();
        // under the lines' lock, which the close takes to clear them after it stops the pool
        if self.lock().stopped {
            drop(lines);
            return drop(job);
        }
        if let Some(waiting) = lines.get_mut(line) {
            return waiting.push_back(job);
        }
        lines.insert(line.to_owned(), VecDeque::new());
        drop(lines);
        let (pool, line) = (Arc::clone(self), line.to_owned());
        self.run(Box::new(move || pool.run_line(&line, job)));
    }

    /// Runs `first`, then each job queued on `line` behind it, until none
    /// waits. A job that panics ends alone, and the line goes on.
    #[allow(dead_code, reason = "only transactions wait in a line: sql's over the wire, and the async API's")]
    fn run_line(&self, line: &str, first: Job) {
        let mut next = Some(first);
        while let Some(job) = next.take() {
            let _ = catch_unwind(AssertUnwindSafe(job));
            let mut lines = self.lock_lines();
            next = lines.get_mut(line).and_then(VecDeque::pop_front);
            if next.is_none() {
                lines.remove(line);
            }
        }
    }

    fn lock_lines(&self) -> MutexGuard<'_, HashMap<String, VecDeque<Job>>> {
        self.lines.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `job` on a thread of the pool; a store that closed drops it unrun,
    /// here, outside the pool's lock, since what a job holds may answer its
    /// caller as it drops.
    pub(crate) fn run(self: &Arc<Self>, job: Job) {
        drop(self.enqueue(job));
    }

    /// Runs `job` on a thread of the pool, or, when it would wait there for a
    /// job under way to end, on a thread started for it: for work that must
    /// run whoever waits for it, as a commit's lead, which the writes queued
    /// behind it wait for. Only a system that starts no thread drops it
    /// unrun; once the pool stopped, as its store closes, it runs alone too.
    ///
    /// Every thread of the pool may wait for what such a job frees. Sixteen
    /// records reads parked for the memory an awaited append holds until it
    /// commits would wait their ten seconds of patience out, and fail
    /// `Limit`, while the lead of that commit waited behind them in the
    /// queue. A blocking append leads its own commit, and never waits so.
    #[cfg(feature = "async")]
    pub(crate) fn run_always(self: &Arc<Self>, job: Job) {
        let mut queue = self.lock();
        if queue.stopped {
            drop(queue);
            let _ = thread::Builder::new().name(ALONE.to_owned()).spawn(job);
            return;
        }
        if !queue.busy() {
            return self.push(queue, job);
        }
        queue.musts.push_back(job);
        // a thread started for them takes it once it ends the one it runs
        if queue.alone == MOST {
            return;
        }
        queue.alone += 1;
        drop(queue);
        self.start_alone();
    }

    /// Starts a thread that runs the jobs that must run until none is left,
    /// joined at the close as the pool's are.
    #[cfg(feature = "async")]
    fn start_alone(self: &Arc<Self>) {
        let pool = Arc::clone(self);
        match thread::Builder::new().name(ALONE.to_owned()).spawn(move || pool.run_musts()) {
            Ok(handle) => self.keep(handle),
            // Dropped unrun once no thread is left for them: each lead then
            // leads where it is dropped, here, since nobody else would.
            Err(_) => {
                let mut queue = self.lock();
                queue.alone -= 1;
                let unrun = if queue.alone == 0 { std::mem::take(&mut queue.musts) } else { VecDeque::new() };
                drop(queue);
                drop(unrun);
            }
        }
    }

    /// Runs the jobs that must run, in order, until none waits. A job that
    /// panics ends alone, and the thread goes on.
    #[cfg(feature = "async")]
    fn run_musts(&self) {
        let mut queue = self.lock();
        while let Some(job) = queue.musts.pop_front() {
            drop(queue);
            let _ = catch_unwind(AssertUnwindSafe(job));
            queue = self.lock();
        }
        queue.alone -= 1;
    }

    /// Queues `job` for a thread of the pool, or hands it back once the pool
    /// stopped.
    fn enqueue(self: &Arc<Self>, job: Job) -> Option<Job> {
        let queue = self.lock();
        if queue.stopped {
            return Some(job);
        }
        self.push(queue, job);
        None
    }

    /// Queues `job` and gets a thread to it: the one that lingers, one
    /// started, or one that sleeps, woken.
    fn push(self: &Arc<Self>, mut queue: MutexGuard<'_, Queue>, job: Job) {
        queue.jobs.push_back(job);
        self.waiting.store(queue.jobs.len(), Ordering::Release);
        let taken = queue.lingering && queue.jobs.len() == 1;
        if !taken && queue.idle == 0 && queue.started < MOST {
            queue.started += 1;
            queue.coming += 1;
            drop(queue);
            return self.start();
        }
        let wakes = queue.wakes_one();
        drop(queue);
        if wakes {
            self.ready.notify_one();
        }
    }

    fn start(self: &Arc<Self>) {
        let pool = Arc::clone(self);
        let spawned = thread::Builder::new().name(THREAD.to_owned()).spawn(move || pool.work());
        match spawned {
            Ok(handle) => self.keep(handle),
            // the jobs queued wait for a thread already running, or for the next start
            Err(_) => {
                let mut queue = self.lock();
                queue.started -= 1;
                queue.coming -= 1;
            }
        }
    }

    /// Keeps a thread started for the close to join.
    fn keep(&self, handle: JoinHandle<()>) {
        let mut threads = self.threads.lock().unwrap_or_else(PoisonError::into_inner);
        // a thread that ended idle is joined here, or the list grows with every burst
        threads.retain(|thread| !thread.is_finished());
        threads.push(handle);
    }

    fn work(self: &Arc<Self>) {
        let mut queue = self.lock();
        queue.coming -= 1;
        loop {
            if let Some(job) = queue.jobs.pop_front() {
                self.waiting.store(queue.jobs.len(), Ordering::Release);
                if let Some(emptied) = queue.emptied.take() {
                    queue.dense = emptied.elapsed() < self.linger;
                }
                let wakes = queue.wakes_one();
                let starts = !wakes && queue.starts_one();
                drop(queue);
                if wakes {
                    self.ready.notify_one();
                }
                if starts {
                    self.start();
                }
                job();
                queue = self.lock();
                if queue.jobs.is_empty() {
                    queue.emptied = Some(Instant::now());
                }
                continue;
            }
            if queue.stopped {
                return;
            }
            if let Some(until) = queue.lingers_until(self.linger) {
                queue = self.linger(queue, until);
                continue;
            }
            queue.idle += 1;
            let (woken, slept) =
                self.ready.wait_timeout(queue, self.retire_after).unwrap_or_else(PoisonError::into_inner);
            queue = woken;
            queue.idle -= 1;
            queue.waking = false;
            // A wake that lands as the sleep ends still finds its job queued,
            // and only a thread that finds none ends: the next job starts one.
            if slept.timed_out() && queue.jobs.is_empty() && !queue.stopped {
                queue.started -= 1;
                return;
            }
        }
    }

    /// Looks for the next job without the lock, and says the flow has thinned
    /// when none came by `until`.
    fn linger<'queue>(&'queue self, mut queue: MutexGuard<'queue, Queue>, until: Instant) -> MutexGuard<'queue, Queue> {
        queue.lingering = true;
        drop(queue);
        let mut came = false;
        while !came && !self.closing.load(Ordering::Acquire) && Instant::now() < until {
            std::hint::spin_loop();
            came = self.waiting.load(Ordering::Acquire) > 0;
        }
        let mut queue = self.lock();
        queue.lingering = false;
        queue.dense = came;
        queue
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Queue {
    /// Until when a thread that found no job looks for the next: the linger
    /// after the queue emptied, while the flow is dense and no thread looks
    /// already.
    fn lingers_until(&self, linger: Duration) -> Option<Instant> {
        let until = self.emptied? + linger;
        (self.dense && !self.lingering && Instant::now() < until).then_some(until)
    }

    /// Whether a job queued now would wait for one under way to end: the pool
    /// runs its most threads, and those that sleep, look for jobs or are on
    /// their way take the jobs queued before it.
    #[cfg(feature = "async")]
    fn busy(&self) -> bool {
        let free = self.idle + self.coming + usize::from(self.lingering);
        self.started >= MOST && self.jobs.len() >= free
    }

    /// Whether to wake a thread: a job waits, a thread sleeps, and no thread
    /// is on its way to the job, woken or lingering. It marks the wake as
    /// under way.
    fn wakes_one(&mut self) -> bool {
        let wakes = !self.jobs.is_empty() && self.idle > 0 && !self.waking && !self.lingering;
        self.waking |= wakes;
        wakes
    }

    /// Whether the thread that took a job starts another for the jobs behind
    /// it: more wait than threads on their way to them, none sleeps or looks
    /// for them, and the job taken may block its thread, a write waiting for
    /// its file's writer, while no call comes to start one. The calls a burst
    /// queued while the one thread slept found it counted idle until it
    /// woke, and started none. A pool that closes starts none. It counts the
    /// thread as started and coming.
    fn starts_one(&mut self) -> bool {
        let starts =
            self.jobs.len() > self.coming && self.idle == 0 && !self.lingering && !self.stopped && self.started < MOST;
        self.started += usize::from(starts);
        self.coming += usize::from(starts);
        starts
    }
}

impl Engine for Pool {
    /// Lets the queued calls run while every engine they reach is open.
    fn stop(&self) {
        let _ = self.close();
    }

    /// Lets the queued calls run, then stops every thread and waits for it.
    fn close(&self) -> Result<()> {
        self.lock().stopped = true;
        self.closing.store(true, Ordering::Release);
        self.ready.notify_all();
        let threads = std::mem::take(&mut *self.threads.lock().unwrap_or_else(PoisonError::into_inner));
        // a call that drops the store's last handle closes it from the pool, whose thread is not joined
        for thread in threads.into_iter().filter(|thread| thread.thread().id() != thread::current().id()) {
            let _ = thread.join();
        }
        // A line whose first job came after the stop runs nothing more. Its
        // jobs are dropped outside the lines' lock, since what one holds may
        // answer its caller as it drops, and a caller woken there may queue
        // another.
        let unrun = std::mem::take(&mut *self.lock_lines());
        drop(unrun);
        Ok(())
    }
}

impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pool({})", self.lock().started)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::Options;

    /// A pool whose threads never end idle within a test.
    fn lingering(linger: Duration) -> Arc<Pool> {
        Arc::new(Pool::new(linger, RETIRE_AFTER))
    }

    /// Queues four jobs that end only once all four run at one time.
    fn run_four_together(pool: &Arc<Pool>) {
        let (together, ended) = (Arc::new(Barrier::new(4)), mpsc::channel());
        for _ in 0..4 {
            let (together, ended) = (Arc::clone(&together), ended.0.clone());
            pool.run(Box::new(move || {
                together.wait();
                ended.send(()).unwrap();
            }));
        }
        for _ in 0..4 {
            ended.1.recv_timeout(Duration::from_secs(10)).expect("four jobs run together");
        }
    }

    /// Runs a job and waits for it to end.
    fn run_one(pool: &Arc<Pool>) {
        let ended = mpsc::channel();
        pool.run(Box::new(move || ended.0.send(()).unwrap()));
        ended.1.recv_timeout(Duration::from_secs(10)).expect("the job runs");
    }

    fn wait_until(what: &str, holds: impl Fn(&Queue) -> bool, pool: &Pool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !holds(&pool.lock()) {
            assert!(Instant::now() < deadline, "{what}");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_thread_that_lingers_takes_the_next_job_with_no_wake_and_no_thread_more() {
        let pool = lingering(Duration::from_secs(60));
        // two threads, which sleep after their first jobs: nothing says yet that jobs come close together
        let (together, ended) = (Arc::new(Barrier::new(2)), mpsc::channel());
        for _ in 0..2 {
            let (together, ended) = (Arc::clone(&together), ended.0.clone());
            pool.run(Box::new(move || {
                together.wait();
                ended.send(()).unwrap();
            }));
        }
        for _ in 0..2 {
            ended.1.recv_timeout(Duration::from_secs(10)).expect("two jobs run together");
        }
        wait_until("the threads never slept", |queue| queue.idle == 2, &pool);

        // the next job came within the linger of the queue's emptying: the thread it woke lingers after it
        run_one(&pool);
        wait_until("no thread lingered", |queue| queue.lingering, &pool);
        assert_eq!(pool.lock().idle, 1, "one thread lingers, the other sleeps");

        pool.run(Box::new(|| {}));
        assert!(!pool.lock().waking, "the job woke nobody");
        wait_until("the thread that lingers took no job", |queue| queue.jobs.is_empty(), &pool);
        let queue = pool.lock();
        assert_eq!((queue.started, queue.idle), (2, 1), "the sleeping thread sleeps on, and none was started");
        drop(queue);
        pool.close().unwrap();
    }

    #[test]
    fn a_thread_sleeps_once_no_job_comes_within_the_linger() {
        let pool = lingering(Duration::from_millis(5));
        run_one(&pool);
        run_one(&pool);
        wait_until("a thread lingered on", |queue| queue.idle == queue.started, &pool);
        // long past the linger, a job says nothing of the next: its thread sleeps at once after it
        thread::sleep(Duration::from_millis(20));
        run_one(&pool);
        wait_until("a thread never slept", |queue| queue.idle == queue.started, &pool);
        assert!(!pool.lock().dense, "jobs this far apart are not worth looking for");
        pool.close().unwrap();
    }

    #[test]
    fn the_jobs_behind_one_that_blocks_its_thread_get_a_thread_of_their_own() {
        let pool = lingering(Duration::ZERO);
        run_one(&pool);
        wait_until("the thread never slept", |queue| queue.idle == 1, &pool);
        // A burst queues its calls while the one thread sleeps: the first wakes
        // it, and the rest find it counted idle still, so they start none. The
        // first then blocks its thread, as a kv write waiting for its file's
        // writer does.
        let (release, released) = mpsc::channel::<()>();
        let (ran, behind) = mpsc::channel();
        {
            let mut queue = pool.lock();
            queue.jobs.push_back(Box::new(move || {
                let _ = released.recv();
            }));
            queue.jobs.push_back(Box::new(move || ran.send(()).unwrap()));
            pool.waiting.store(queue.jobs.len(), Ordering::Release);
            assert!(queue.wakes_one());
        }
        pool.ready.notify_one();
        let behind = behind.recv_timeout(Duration::from_secs(10));
        release.send(()).unwrap();
        behind.expect("the job behind one that blocks its thread never ran");
        pool.close().unwrap();
    }

    #[test]
    fn jobs_queued_for_sleeping_threads_wake_them_one_through_another() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        let pool = Pool::of(&store).unwrap();
        // the first four find no thread and start one each
        run_four_together(&pool);
        let deadline = Instant::now() + Duration::from_secs(10);
        while pool.lock().idle < 4 {
            assert!(Instant::now() < deadline, "the threads never slept");
            thread::sleep(Duration::from_millis(1));
        }
        // the next four find them asleep: the first wakes one, which wakes the next
        run_four_together(&pool);
        assert_eq!(pool.lock().started, 4, "no thread more was started");
        store.close().unwrap();
    }

    #[test]
    fn a_thread_that_slept_its_while_without_a_job_ends_and_the_next_job_starts_one() {
        let pool = Arc::new(Pool::new(Duration::ZERO, Duration::from_millis(20)));
        run_four_together(&pool);
        wait_until("an idle thread lived on", |queue| queue.started == 0 && queue.idle == 0, &pool);
        let ended = |pool: &Pool| pool.threads.lock().unwrap().iter().filter(|thread| thread.is_finished()).count();
        let deadline = Instant::now() + Duration::from_secs(10);
        while ended(&pool) < 4 {
            assert!(Instant::now() < deadline, "the idle threads never ended");
            thread::sleep(Duration::from_millis(1));
        }

        run_one(&pool);
        assert_eq!(pool.threads.lock().unwrap().len(), 1, "the ended threads were let go as one started");
        pool.close().unwrap();
    }

    /// Runs its closure as it is dropped, as an awaited call's answer wakes
    /// its caller when its job is dropped unrun.
    struct OnDrop<F: FnOnce()>(Option<F>);

    impl<F: FnOnce()> Drop for OnDrop<F> {
        fn drop(&mut self) {
            if let Some(dropped) = self.0.take() {
                dropped();
            }
        }
    }

    #[test]
    fn a_line_of_a_stopped_pool_drops_every_job_it_is_given_at_once() {
        let pool = lingering(LINGER);
        pool.close().unwrap();
        // the first job began the line, which no thread would ever end, and the next waited behind it
        for _ in 0..2 {
            let (told, dropped) = mpsc::channel();
            let answer = OnDrop(Some(move || {
                let _ = told.send(());
            }));
            pool.run_in_line("store", Box::new(move || drop(answer)));
            dropped.try_recv().expect("the job waits on a line that never runs");
        }
        assert!(pool.lock_lines().is_empty());
    }

    #[test]
    fn a_closing_pool_drops_the_jobs_left_on_its_lines_outside_their_lock() {
        let pool = lingering(LINGER);
        // a job left on a line, whose drop wakes a caller that queues another at once
        let (told, dropped) = mpsc::channel();
        let again = Arc::clone(&pool);
        let answer = OnDrop(Some(move || {
            again.run_in_line("store", Box::new(|| {}));
            let _ = told.send(());
        }));
        let left: Job = Box::new(move || drop(answer));
        pool.lock_lines().insert("store".to_owned(), VecDeque::from([left]));
        let closing = {
            let pool = Arc::clone(&pool);
            thread::spawn(move || pool.close().unwrap())
        };
        dropped.recv_timeout(Duration::from_secs(10)).expect("the close dropped the job under the lines' lock");
        closing.join().unwrap();
    }

    /// What a job waits for until another job frees it, ten seconds at most:
    /// a read parked for the memory a commit gives back.
    #[cfg(feature = "async")]
    #[derive(Default)]
    struct Freed {
        free: Mutex<bool>,
        changed: Condvar,
    }

    #[cfg(feature = "async")]
    impl Freed {
        fn free(&self) {
            *self.free.lock().unwrap() = true;
            self.changed.notify_all();
        }

        /// Says whether it was freed before the ten seconds were out.
        fn wait(&self) -> bool {
            let free = self.free.lock().unwrap();
            let waited = self.changed.wait_timeout_while(free, Duration::from_secs(10), |free| !*free).unwrap();
            !waited.1.timed_out()
        }
    }

    // contract: STORE-10
    #[cfg(feature = "async")]
    #[test]
    fn a_job_that_must_run_while_every_thread_waits_for_it_runs_on_a_thread_of_its_own() {
        let pool = lingering(LINGER);
        let (freed, (running, ran), (ended, waited)) = (Arc::new(Freed::default()), mpsc::channel(), mpsc::channel());
        for _ in 0..MOST {
            let (freed, running, ended) = (Arc::clone(&freed), running.clone(), ended.clone());
            pool.run(Box::new(move || {
                running.send(()).unwrap();
                ended.send(freed.wait()).unwrap();
            }));
        }
        for _ in 0..MOST {
            ran.recv_timeout(Duration::from_secs(10)).expect("every thread of the pool runs a job");
        }
        assert!(pool.lock().busy());

        // a commit's lead, which frees them all
        let (named, name) = mpsc::channel();
        let frees = Arc::clone(&freed);
        pool.run_always(Box::new(move || {
            named.send(thread::current().name().map(str::to_owned)).unwrap();
            frees.free();
        }));
        let ran_on = name.recv_timeout(Duration::from_secs(5)).expect("the lead waited behind the jobs it frees");
        assert_eq!(ran_on.as_deref(), Some(ALONE));
        for _ in 0..MOST {
            assert!(waited.recv_timeout(Duration::from_secs(10)).unwrap(), "a job waited its patience out");
        }
        wait_until("the thread started for the lead lived on", |queue| queue.alone == 0, &pool);

        // with a thread of the pool free, a lead runs there
        wait_until("no thread of the pool came back", |queue| !queue.busy(), &pool);
        let (named, name) = mpsc::channel();
        pool.run_always(Box::new(move || named.send(thread::current().name().map(str::to_owned)).unwrap()));
        assert_eq!(name.recv_timeout(Duration::from_secs(10)).unwrap().as_deref(), Some(THREAD));
        pool.close().unwrap();
    }

    #[test]
    fn a_job_runs_on_a_thread_named_for_the_pool() {
        let pool = lingering(LINGER);
        let (named, name) = mpsc::channel();
        pool.run(Box::new(move || named.send(thread::current().name().map(str::to_owned)).unwrap()));
        assert_eq!(name.recv_timeout(Duration::from_secs(10)).unwrap().as_deref(), Some(THREAD));
        pool.close().unwrap();
    }
}
