use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::{Error, Result};

type Work = Box<dyn FnMut() -> Result<()> + Send>;

/// The store's periodic work, run one task at a time on one thread of the
/// store's own, which starts with the first task and stops at close.
pub(crate) struct Scheduler {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
    background: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    tasks: Vec<Task>,
    /// The id the next task added takes.
    next_id: u64,
    stopped: bool,
}

struct Task {
    id: u64,
    name: String,
    period: Duration,
    next: Instant,
    /// None while the task runs, outside the lock.
    work: Option<Work>,
    failing: bool,
    /// Woken while it ran: it runs again at once rather than a period later.
    woken: bool,
}

/// What runs a task of the store's background work now, rather than at its
/// next period: a logger whose buffer fills sooner than its period. A task
/// woken while it runs runs again once it ends.
#[derive(Clone, Debug)]
pub struct Wake {
    shared: Weak<Shared>,
    id: u64,
}

impl Wake {
    pub fn wake(&self) {
        let Some(shared) = self.shared.upgrade() else { return };
        let mut state = shared.lock();
        if let Some(task) = state.tasks.iter_mut().find(|task| task.id == self.id) {
            if task.work.is_some() {
                task.next = Instant::now();
            } else {
                task.woken = true;
            }
        }
        drop(state);
        shared.changed.notify_all();
    }
}

impl Scheduler {
    pub(crate) fn new(background: bool) -> Self {
        Self {
            shared: Arc::new(Shared { state: Mutex::new(State::default()), changed: Condvar::new() }),
            thread: Mutex::new(None),
            background,
        }
    }

    /// Runs `work` every `period`, and when what it gives back wakes it.
    /// Without background work nothing runs, and a wake does nothing: the
    /// application calls each engine's maintenance itself.
    pub(crate) fn add(&self, name: &str, period: Duration, work: Work) -> Result<Wake> {
        if period.is_zero() {
            return Err(Error::invalid(format!("background work {name}: a period of zero")));
        }
        if !self.background {
            return Ok(Wake { shared: Weak::new(), id: 0 });
        }
        let mut state = self.shared.lock();
        if state.stopped {
            return Err(Error::closed(format!("background work {name}")));
        }
        let id = state.next_id;
        state.next_id += 1;
        state.tasks.push(Task {
            id,
            name: name.to_owned(),
            period,
            next: Instant::now() + period,
            work: Some(work),
            failing: false,
            woken: false,
        });
        drop(state);
        self.start()?;
        self.shared.changed.notify_all();
        Ok(Wake { shared: Arc::downgrade(&self.shared), id })
    }

    fn start(&self) -> Result<()> {
        let mut thread = self.thread.lock().unwrap_or_else(PoisonError::into_inner);
        if thread.is_some() {
            return Ok(());
        }
        let shared = Arc::clone(&self.shared);
        let handle = thread::Builder::new()
            .name("tinystore-background".to_owned())
            .spawn(move || shared.run())
            .map_err(|error| Error::io("background work: its thread", error))?;
        *thread = Some(handle);
        Ok(())
    }

    /// Stops the thread after the task it runs, if any, and waits for it.
    pub(crate) fn stop(&self) {
        self.shared.lock().stopped = true;
        self.shared.changed.notify_all();
        let handle = self.thread.lock().unwrap_or_else(PoisonError::into_inner).take();
        // A task that drops the store's last handle stops the scheduler from its
        // own thread, which ends once the task returns rather than being joined.
        if let Some(handle) = handle.filter(|handle| handle.thread().id() != thread::current().id()) {
            // A panic in a task was caught where it ran; this join cannot carry one.
            let _ = handle.join();
        }
    }
}

impl Shared {
    fn run(&self) {
        let mut state = self.lock();
        while !state.stopped {
            let now = Instant::now();
            match state.tasks.iter().position(|task| task.work.is_some() && task.next <= now) {
                Some(due) => state = self.run_task(state, due),
                None => state = self.wait_for_next(state, now),
            }
        }
    }

    fn run_task<'a>(&'a self, mut state: MutexGuard<'a, State>, due: usize) -> MutexGuard<'a, State> {
        let mut work = state.tasks[due].work.take().expect("a due task holds its work");
        drop(state);
        let outcome = catch_unwind(AssertUnwindSafe(&mut work)).unwrap_or_else(|_| Err(Error::internal("it panicked")));
        let mut state = self.lock();
        let task = &mut state.tasks[due];
        task.report(outcome);
        task.work = Some(work);
        let wait = if std::mem::take(&mut task.woken) { Duration::ZERO } else { task.period };
        task.next = Instant::now() + wait;
        state
    }

    fn wait_for_next<'a>(&'a self, state: MutexGuard<'a, State>, now: Instant) -> MutexGuard<'a, State> {
        let next = state.tasks.iter().filter(|task| task.work.is_some()).map(|task| task.next).min();
        match next {
            Some(next) => {
                let wait = next.saturating_duration_since(now);
                self.changed.wait_timeout(state, wait).unwrap_or_else(PoisonError::into_inner).0
            }
            None => self.changed.wait(state).unwrap_or_else(PoisonError::into_inner),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Task {
    /// Logs a failure once and a recovery once, not every period.
    fn report(&mut self, outcome: Result<()>) {
        match outcome {
            Err(error) if !self.failing => {
                self.failing = true;
                tracing::warn!(target: "tinystore", work = %self.name, %error, "background work failed");
            }
            Ok(()) if self.failing => {
                self.failing = false;
                tracing::info!(target: "tinystore", work = %self.name, "background work recovered");
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counting() -> (Arc<AtomicUsize>, Work) {
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&runs);
        let work: Work = Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        (runs, work)
    }

    fn wait_until(condition: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            thread::sleep(Duration::from_millis(1));
        }
        false
    }

    #[test]
    fn work_runs_every_period_until_the_scheduler_stops() {
        let scheduler = Scheduler::new(true);
        let (runs, work) = counting();
        scheduler.add("test: tick", Duration::from_millis(5), work).unwrap();
        assert!(wait_until(|| runs.load(Ordering::SeqCst) >= 3));
        scheduler.stop();
        let after = runs.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(30));
        assert_eq!(runs.load(Ordering::SeqCst), after);
    }

    #[test]
    fn without_background_work_nothing_runs_and_no_thread_starts() {
        let scheduler = Scheduler::new(false);
        let (runs, work) = counting();
        scheduler.add("test: tick", Duration::from_millis(1), work).unwrap();
        thread::sleep(Duration::from_millis(20));
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        assert!(scheduler.thread.lock().unwrap().is_none());
    }

    #[test]
    fn a_task_that_panics_keeps_running_on_its_period() {
        let scheduler = Scheduler::new(true);
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&runs);
        let work: Work = Box::new(move || {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("first run");
            }
            Ok(())
        });
        scheduler.add("test: flaky", Duration::from_millis(5), work).unwrap();
        assert!(wait_until(|| runs.load(Ordering::SeqCst) >= 2));
        scheduler.stop();
    }

    #[test]
    fn a_woken_task_runs_before_its_period_and_again_when_woken_while_it_runs() {
        let scheduler = Scheduler::new(true);
        let (started, finished) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (counter, done) = (Arc::clone(&started), Arc::clone(&finished));
        let work: Work = Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(50));
            done.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let wake = scheduler.add("test: woken", Duration::from_secs(3600), work).unwrap();
        wake.wake();
        assert!(wait_until(|| started.load(Ordering::SeqCst) == 1), "a wake runs it before its hour");
        wake.wake();
        assert!(wait_until(|| finished.load(Ordering::SeqCst) == 2), "woken while it ran, it runs again");
        thread::sleep(Duration::from_millis(100));
        assert_eq!(started.load(Ordering::SeqCst), 2, "and then waits for its period");
        scheduler.stop();
    }

    #[test]
    fn a_wake_without_background_work_does_nothing() {
        let scheduler = Scheduler::new(false);
        let (runs, work) = counting();
        scheduler.add("test: tick", Duration::from_millis(1), work).unwrap().wake();
        thread::sleep(Duration::from_millis(20));
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn work_added_after_stop_is_refused() {
        let scheduler = Scheduler::new(true);
        scheduler.stop();
        let (_, work) = counting();
        let error = scheduler.add("test: late", Duration::from_millis(5), work).unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Closed);
    }
}
