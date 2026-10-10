use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

use crate::Result;
use crate::Store;
use crate::engine::{Engine, Host};

/// Threads a store's connections run their calls on, at most this many: a
/// write blocks its thread until its group commits, and the threads waiting
/// together are the writes one commit can carry.
const MOST: usize = 16;

type Job = Box<dyn FnOnce() + Send>;

/// The store's threads for the calls connections send: started as calls need
/// them, kept until the store closes, which waits for the calls under way.
pub(crate) struct Workers {
    queue: Mutex<Queue>,
    ready: Condvar,
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
    started: usize,
    stopped: bool,
}

impl Workers {
    pub(crate) fn of(store: &Store) -> Result<Arc<Workers>> {
        store.engine(|_| {
            Ok(Arc::new(Workers {
                queue: Mutex::new(Queue::default()),
                ready: Condvar::new(),
                threads: Mutex::new(Vec::new()),
            }))
        })
    }

    /// Runs `job` on a worker; a store that closed drops it unrun.
    pub(crate) fn run(self: &Arc<Self>, job: Job) {
        let mut queue = self.lock();
        if queue.stopped {
            return;
        }
        queue.jobs.push_back(job);
        if queue.idle == 0 && queue.started < MOST {
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
                let wakes = queue.wakes_one();
                drop(queue);
                if wakes {
                    self.ready.notify_one();
                }
                job();
                queue = self.lock();
                continue;
            }
            if queue.stopped {
                return;
            }
            queue.idle += 1;
            queue = self.ready.wait(queue).unwrap_or_else(PoisonError::into_inner);
            queue.idle -= 1;
            queue.waking = false;
        }
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Queue {
    /// Whether to wake a thread: a job waits, a thread sleeps, and no wake is
    /// under way. It marks the wake as under way.
    fn wakes_one(&mut self) -> bool {
        let wakes = !self.jobs.is_empty() && self.idle > 0 && !self.waking;
        self.waking |= wakes;
        wakes
    }
}

impl Engine for Workers {
    /// Lets the queued calls run, then stops every thread and waits for it.
    fn close(&self) -> Result<()> {
        self.lock().stopped = true;
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
