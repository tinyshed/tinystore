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
            self.start();
        } else {
            drop(queue);
        }
        self.ready.notify_one();
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
                drop(queue);
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
        }
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Engine for Workers {
    /// Lets the queued calls run, then stops every thread and waits for it.
    fn close(&self) -> Result<()> {
        self.lock().stopped = true;
        self.ready.notify_all();
        let threads = std::mem::take(&mut *self.threads.lock().unwrap_or_else(PoisonError::into_inner));
        for thread in threads {
            let _ = thread.join();
        }
        Ok(())
    }
}

impl std::fmt::Debug for Workers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Workers({})", self.lock().started)
    }
}
