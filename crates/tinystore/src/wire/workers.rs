use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::Result;
use crate::Store;
use crate::engine::{Engine, Host};

/// Threads a store's connections run their calls on, at most this many: a
/// write blocks its thread until its group commits, and the threads waiting
/// together are the writes one commit can carry.
pub(super) const MOST: usize = 16;
/// How long a thread that found no job looks for the next before it sleeps.
/// Waking a thread that sleeps costs more than a point read, and under a
/// hypervisor ten times more: a read through a worker took 52 microseconds
/// where the worker slept between two, and 10 where it had not.
const LINGER: Duration = Duration::from_micros(200);

type Job = Box<dyn FnOnce() + Send>;

/// The store's threads for the calls connections send: started as calls need
/// them, kept until the store closes, which waits for the calls under way.
pub(crate) struct Workers {
    queue: Mutex<Queue>,
    ready: Condvar,
    /// Jobs waiting, which the thread that lingers reads without the lock.
    waiting: AtomicUsize,
    closing: AtomicBool,
    linger: Duration,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    idle: usize,
    /// Whether a thread was woken and has not taken its job yet. One wake is
    /// under way at a time: whoever queues a job wakes one thread, and that
    /// thread wakes the next when jobs still wait. A wake a job cost the
    /// thread that reads a connection more than the job itself, and with 64
    /// calls in flight that thread is all the connection has.
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
    stopped: bool,
}

impl Workers {
    pub(crate) fn of(store: &Store) -> Result<Arc<Workers>> {
        store.engine(|_| Ok(Arc::new(Workers::lingering(LINGER))))
    }

    fn lingering(linger: Duration) -> Workers {
        Workers {
            queue: Mutex::new(Queue::default()),
            ready: Condvar::new(),
            waiting: AtomicUsize::new(0),
            closing: AtomicBool::new(false),
            linger,
            threads: Mutex::new(Vec::new()),
        }
    }

    /// Runs `job` on a worker; a store that closed drops it unrun.
    pub(crate) fn run(self: &Arc<Self>, job: Job) {
        let mut queue = self.lock();
        if queue.stopped {
            return;
        }
        queue.jobs.push_back(job);
        self.waiting.store(queue.jobs.len(), Ordering::Release);
        let taken = queue.lingering && queue.jobs.len() == 1;
        if !taken && queue.idle == 0 && queue.started < MOST {
            queue.started += 1;
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
        let workers = Arc::clone(self);
        let spawned = thread::Builder::new().name("tinystore-worker".to_owned()).spawn(move || workers.work());
        match spawned {
            Ok(handle) => self.threads.lock().unwrap_or_else(PoisonError::into_inner).push(handle),
            // the jobs queued wait for a thread already running, or for the next start
            Err(_) => self.lock().started -= 1,
        }
    }

    fn work(&self) {
        let mut queue = self.lock();
        loop {
            if let Some(job) = queue.jobs.pop_front() {
                self.waiting.store(queue.jobs.len(), Ordering::Release);
                if let Some(emptied) = queue.emptied.take() {
                    queue.dense = emptied.elapsed() < self.linger;
                }
                let wakes = queue.wakes_one();
                drop(queue);
                if wakes {
                    self.ready.notify_one();
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
            queue = self.ready.wait(queue).unwrap_or_else(PoisonError::into_inner);
            queue.idle -= 1;
            queue.waking = false;
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

    /// Whether to wake a thread: a job waits, a thread sleeps, and no thread
    /// is on its way to the job, woken or lingering. It marks the wake as
    /// under way.
    fn wakes_one(&mut self) -> bool {
        let wakes = !self.jobs.is_empty() && self.idle > 0 && !self.waking && !self.lingering;
        self.waking |= wakes;
        wakes
    }
}

impl Engine for Workers {
    /// Lets the queued calls run, then stops every thread and waits for it.
    fn close(&self) -> Result<()> {
        self.lock().stopped = true;
        self.closing.store(true, Ordering::Release);
        self.ready.notify_all();
        let threads = std::mem::take(&mut *self.threads.lock().unwrap_or_else(PoisonError::into_inner));
        // a call that drops the store's last handle closes it from a worker, which is not joined
        for worker in threads.into_iter().filter(|worker| worker.thread().id() != thread::current().id()) {
            let _ = worker.join();
        }
        Ok(())
    }
}

impl std::fmt::Debug for Workers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Workers({})", self.lock().started)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::Options;

    /// Queues four jobs that end only once all four run at one time.
    fn run_four_together(workers: &Arc<Workers>) {
        let (together, ended) = (Arc::new(Barrier::new(4)), mpsc::channel());
        for _ in 0..4 {
            let (together, ended) = (Arc::clone(&together), ended.0.clone());
            workers.run(Box::new(move || {
                together.wait();
                ended.send(()).unwrap();
            }));
        }
        for _ in 0..4 {
            ended.1.recv_timeout(Duration::from_secs(10)).expect("four jobs run together");
        }
    }

    /// Runs a job and waits for it to end.
    fn run_one(workers: &Arc<Workers>) {
        let ended = mpsc::channel();
        workers.run(Box::new(move || ended.0.send(()).unwrap()));
        ended.1.recv_timeout(Duration::from_secs(10)).expect("the job runs");
    }

    fn wait_until(what: &str, holds: impl Fn(&Queue) -> bool, workers: &Workers) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !holds(&workers.lock()) {
            assert!(Instant::now() < deadline, "{what}");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_thread_that_lingers_takes_the_next_job_with_no_wake_and_no_thread_more() {
        let workers = Arc::new(Workers::lingering(Duration::from_secs(60)));
        // two threads, which sleep after their first jobs: nothing says yet that jobs come close together
        let (together, ended) = (Arc::new(Barrier::new(2)), mpsc::channel());
        for _ in 0..2 {
            let (together, ended) = (Arc::clone(&together), ended.0.clone());
            workers.run(Box::new(move || {
                together.wait();
                ended.send(()).unwrap();
            }));
        }
        for _ in 0..2 {
            ended.1.recv_timeout(Duration::from_secs(10)).expect("two jobs run together");
        }
        wait_until("the threads never slept", |queue| queue.idle == 2, &workers);

        // the next job came within the linger of the queue's emptying: the thread it woke lingers after it
        run_one(&workers);
        wait_until("no thread lingered", |queue| queue.lingering, &workers);
        assert_eq!(workers.lock().idle, 1, "one thread lingers, the other sleeps");

        workers.run(Box::new(|| {}));
        assert!(!workers.lock().waking, "the job woke nobody");
        wait_until("the thread that lingers took no job", |queue| queue.jobs.is_empty(), &workers);
        let queue = workers.lock();
        assert_eq!((queue.started, queue.idle), (2, 1), "the sleeping thread sleeps on, and none was started");
        drop(queue);
        workers.close().unwrap();
    }

    #[test]
    fn a_thread_sleeps_once_no_job_comes_within_the_linger() {
        let workers = Arc::new(Workers::lingering(Duration::from_millis(5)));
        run_one(&workers);
        run_one(&workers);
        wait_until("a thread lingered on", |queue| queue.idle == queue.started, &workers);
        // long past the linger, a job says nothing of the next: its thread sleeps at once after it
        thread::sleep(Duration::from_millis(20));
        run_one(&workers);
        wait_until("a thread never slept", |queue| queue.idle == queue.started, &workers);
        assert!(!workers.lock().dense, "jobs this far apart are not worth looking for");
        workers.close().unwrap();
    }

    #[test]
    fn jobs_queued_for_sleeping_threads_wake_them_one_through_another() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        let workers = Workers::of(&store).unwrap();
        // the first four find no thread and start one each
        run_four_together(&workers);
        let deadline = Instant::now() + Duration::from_secs(10);
        while workers.lock().idle < 4 {
            assert!(Instant::now() < deadline, "the threads never slept");
            thread::sleep(Duration::from_millis(1));
        }
        // the next four find them asleep: the first wakes one, which wakes the next
        run_four_together(&workers);
        assert_eq!(workers.lock().started, 4, "no thread more was started");
        store.close().unwrap();
    }
}
