//! Handlers' futures polled inside one future, as a queue's worker runs its
//! jobs' handlers and an MCP server its calls' handlers, on the task the
//! program spawns or awaits, spawning nothing.
//!
//! Each handler's future gets a waker of its own, which marks it woken and
//! wakes the task, so that a poll polls only the handlers woken since the
//! last. What else wakes that task, the jobs a loop hands over or the lines
//! a thread reads, is the poller's own state `S`, posted beside in
//! [`Shared`] under the same lock.

use std::collections::HashMap;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;

use crate::sync::lock;

/// A handler's future, its type erased.
pub(crate) type Handler<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// What the task that polls the handlers shares with their wakers and with
/// the threads beside it.
#[derive(Default)]
pub(crate) struct Shared<S> {
    state: Mutex<Posted<S>>,
}

#[derive(Default)]
struct Posted<S> {
    state: S,
    /// The handlers whose waker woke since the task last looked.
    woken: Vec<u64>,
    /// The task that polls the handlers, woken by the next post.
    waker: Option<Waker>,
}

impl<S> Shared<S> {
    /// Changes the poller's state, then wakes its task, outside the lock.
    pub(crate) fn post<R>(&self, change: impl FnOnce(&mut S) -> R) -> R {
        self.changed(|posted| change(&mut posted.state))
    }

    /// Changes what is posted under the lock, then wakes the task outside it.
    fn changed<R>(&self, change: impl FnOnce(&mut Posted<S>) -> R) -> R {
        let (changed, waker) = {
            let mut posted = lock(&self.state);
            (change(&mut posted), posted.waker.take())
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        changed
    }

    /// Reads or changes the poller's state, waking nothing.
    pub(crate) fn with<R>(&self, read: impl FnOnce(&mut S) -> R) -> R {
        read(&mut lock(&self.state).state)
    }

    /// The handlers woken since the last look and what `take` takes of the
    /// state, keeping `waker` to wake at the next post.
    pub(crate) fn look<R>(&self, waker: &Waker, take: impl FnOnce(&mut S) -> R) -> (Vec<u64>, R) {
        let mut posted = lock(&self.state);
        if !posted.waker.as_ref().is_some_and(|kept| kept.will_wake(waker)) {
            posted.waker = Some(waker.clone());
        }
        let taken = take(&mut posted.state);
        (std::mem::take(&mut posted.woken), taken)
    }

    /// Forgets the task's waker, once its future is gone, so that nothing
    /// posted after keeps the task.
    #[cfg(feature = "mcp")]
    pub(crate) fn forget_task(&self) {
        lock(&self.state).waker = None;
    }

    fn woke(&self, handler: u64) {
        self.changed(|posted| {
            if !posted.woken.contains(&handler) {
                posted.woken.push(handler);
            }
        });
    }
}

/// The handlers under way, each by its number, with what it runs for: a job's
/// lease, an MCP call.
pub(crate) struct Handlers<D, T> {
    running: HashMap<u64, Handling<D, T>>,
    last: u64,
}

struct Handling<D, T> {
    with: D,
    future: Handler<T>,
    waker: Waker,
}

/// What a handler ran for and how it ended, a panic among its ends.
pub(crate) type Ended<D, T> = (D, thread::Result<T>);

impl<D, T> Default for Handlers<D, T> {
    fn default() -> Self {
        Handlers { running: HashMap::new(), last: 0 }
    }
}

impl<D, T> Handlers<D, T> {
    /// Begins the handler `make` makes, run for `with`, its waker marking it
    /// woken in `shared`, and polls it a first time: how it ended, when it
    /// did at once or `make` panicked.
    pub(crate) fn begin<S: Send + 'static>(
        &mut self,
        with: D,
        make: impl FnOnce() -> Handler<T>,
        shared: &Arc<Shared<S>>,
    ) -> Option<Ended<D, T>> {
        let future = match catch_unwind(AssertUnwindSafe(make)) {
            Ok(future) => future,
            Err(panic) => return Some((with, Err(panic))),
        };
        self.last += 1;
        let number = self.last;
        let waker = Waker::from(Arc::new(Slot { number, shared: Arc::downgrade(shared) }));
        self.running.insert(number, Handling { with, future, waker });
        self.poll(number)
    }

    /// Polls the handler `number` with its own waker: how it ended once it
    /// has, its future dropped first; none while it runs, or once it is gone.
    pub(crate) fn poll(&mut self, number: u64) -> Option<Ended<D, T>> {
        let handling = self.running.get_mut(&number)?;
        let mut context = Context::from_waker(&handling.waker);
        let ran = match catch_unwind(AssertUnwindSafe(|| handling.future.as_mut().poll(&mut context))) {
            Ok(Poll::Pending) => return None,
            Ok(Poll::Ready(answer)) => Ok(answer),
            Err(panic) => Err(panic),
        };
        let Handling { with, future, .. } = self.running.remove(&number)?;
        drop(future);
        Some((with, ran))
    }

    #[cfg(feature = "jobs")]
    pub(crate) fn len(&self) -> usize {
        self.running.len()
    }

    #[cfg(feature = "mcp")]
    pub(crate) fn is_empty(&self) -> bool {
        self.running.is_empty()
    }

    /// Drops the futures of the handlers `stops` picks, then gives what each
    /// ran for.
    pub(crate) fn stop(&mut self, stops: impl Fn(&D) -> bool) -> Vec<D> {
        let picked: Vec<u64> =
            self.running.iter().filter(|(_, handling)| stops(&handling.with)).map(|(at, _)| *at).collect();
        picked.into_iter().filter_map(|number| self.running.remove(&number)).map(|handling| handling.with).collect()
    }
}

/// One handler's waker: it marks its handler woken and wakes the task, so
/// that the task polls that handler alone. It holds what it shares weakly,
/// since that outlives no poller.
struct Slot<S> {
    number: u64,
    shared: Weak<Shared<S>>,
}

impl<S: Send + 'static> Wake for Slot<S> {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(shared) = self.shared.upgrade() {
            shared.woke(self.number);
        }
    }
}
