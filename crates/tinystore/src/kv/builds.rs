//! The threads a store's blocking builds in the background run on. A build
//! runs the program's function, which takes as long as its source does: on
//! the store's pool, sixteen slow ones held every thread the pipe's calls and
//! the async API's run on, and a build that read the store through one of
//! those waited behind itself.
//!
//! Started as builds come, four at most, each ended once it slept a minute
//! without one, and all stopped as the store stops: the builds waiting are
//! dropped, which lets their keys go, and those under way are waited for.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::engine::{Engine, Host};
use crate::{Result, Store};

/// Builds that run at once at most; those after them wait for a thread.
pub(crate) const MOST_BUILDS: usize = 4;
/// How long a thread sleeps without a build before it ends.
const RETIRE_AFTER: Duration = Duration::from_secs(60);
/// The name of every thread a build runs on.
pub(crate) const THREAD: &str = "tinystore-build";

type Build = Box<dyn FnOnce() + Send>;

#[derive(Default)]
pub(crate) struct Builds {
    queue: Mutex<Queue>,
    ready: Condvar,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

#[derive(Default)]
struct Queue {
    builds: VecDeque<Build>,
    idle: usize,
    started: usize,
    stopped: bool,
}

impl Builds {
    /// The store's builds, which start no thread before the first.
    pub(crate) fn of(store: &Store) -> Result<Arc<Builds>> {
        store.engine(|_| Ok(Arc::new(Builds::default())))
    }

    /// Runs `build` on a thread of these, once one is free; a store that
    /// stopped drops it unrun, outside the lock, since dropping it lets its
    /// key go and wakes whoever waits for that.
    pub(crate) fn run(self: &Arc<Self>, build: Build) {
        let mut queue = self.lock();
        if queue.stopped {
            drop(queue);
            return drop(build);
        }
        queue.builds.push_back(build);
        if queue.idle > 0 {
            drop(queue);
            return self.ready.notify_one();
        }
        if queue.started == MOST_BUILDS {
            return;
        }
        queue.started += 1;
        drop(queue);
        self.start();
    }

    fn start(self: &Arc<Self>) {
        let builds = Arc::clone(self);
        match thread::Builder::new().name(THREAD.to_owned()).spawn(move || builds.work()) {
            Ok(handle) => {
                let mut threads = self.threads.lock().unwrap_or_else(PoisonError::into_inner);
                // a thread that ended idle is joined here, or the list grows with every burst
                threads.retain(|thread| !thread.is_finished());
                threads.push(handle);
            }
            // the builds queued wait for a thread running, or for the next build to start one
            Err(_) => self.lock().started -= 1,
        }
    }

    /// Runs builds until none comes for a minute, or the store stops.
    fn work(&self) {
        let mut queue = self.lock();
        loop {
            if let Some(build) = queue.builds.pop_front() {
                drop(queue);
                // a build catches its function's panic; one of its own ends it alone
                let _ = catch_unwind(AssertUnwindSafe(build));
                queue = self.lock();
                continue;
            }
            if queue.stopped {
                queue.started -= 1;
                return;
            }
            queue.idle += 1;
            let (woken, slept) = self.ready.wait_timeout(queue, RETIRE_AFTER).unwrap_or_else(PoisonError::into_inner);
            queue = woken;
            queue.idle -= 1;
            if slept.timed_out() && queue.builds.is_empty() {
                queue.started -= 1;
                return;
            }
        }
    }

    /// How many threads run builds now.
    #[cfg(test)]
    pub(crate) fn started(&self) -> usize {
        self.lock().started
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Engine for Builds {
    /// Drops the builds waiting, which lets their keys go, and waits for those
    /// under way: the program's functions, which end while every file they
    /// write is open.
    fn stop(&self) {
        let waiting = {
            let mut queue = self.lock();
            queue.stopped = true;
            std::mem::take(&mut queue.builds)
        };
        self.ready.notify_all();
        drop(waiting);
        let threads = std::mem::take(&mut *self.threads.lock().unwrap_or_else(PoisonError::into_inner));
        // a build that drops the store's last handle closes it from its own thread, which is not joined
        for thread in threads.into_iter().filter(|thread| thread.thread().id() != thread::current().id()) {
            let _ = thread.join();
        }
    }

    fn close(&self) -> Result<()> {
        self.stop();
        Ok(())
    }
}

impl std::fmt::Debug for Builds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Builds({})", self.lock().started)
    }
}
