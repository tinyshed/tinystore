//! The store's timer: one thread that rings each alarm when its time comes,
//! so that what waits with a deadline holds no thread of its own. A
//! reservation of memory an awaited call waits for gives up here, its
//! patience over, while the executor that polls it goes on.
//!
//! The thread starts with the first alarm and ends once it slept a minute
//! with none, or when the store closes; an alarm still set then rings
//! [`Rang::Stopped`], so that nothing waits for a time that never comes.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How long the thread sleeps with no alarm before it ends.
const RETIRE_AFTER: Duration = Duration::from_secs(60);

/// What an alarm runs: on the timer's thread when its time came, or where
/// the timer stopped. It must not block, since every alarm after it waits.
pub(crate) type Alarm = Box<dyn FnOnce(Rang) + Send>;

/// Why an alarm rang.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rang {
    /// Its time came.
    Due,
    /// The timer stopped first, with its store.
    Stopped,
}

pub(crate) struct Timer {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    /// By when each rings, then the order they were set in.
    alarms: BTreeMap<(Instant, u64), Alarm>,
    next: u64,
    running: bool,
    stopped: bool,
}

/// An alarm set, which [`Set::cancel`] takes back unrung.
#[derive(Debug)]
pub(crate) struct Set {
    shared: Weak<Shared>,
    key: (Instant, u64),
}

impl Timer {
    pub(crate) fn new() -> Timer {
        let shared = Arc::new(Shared { state: Mutex::default(), changed: Condvar::new() });
        Timer { shared, thread: Mutex::new(None) }
    }

    /// Rings `alarm` once `at` comes, on the timer's thread. A timer that
    /// stopped rings it `Stopped` at once, here.
    pub(crate) fn at(&self, at: Instant, alarm: impl FnOnce(Rang) + Send + 'static) -> Set {
        let mut state = self.shared.lock();
        let key = (at, state.next);
        state.next += 1;
        if state.stopped {
            drop(state);
            alarm(Rang::Stopped);
            return Set { shared: Weak::new(), key };
        }
        state.alarms.insert(key, Box::new(alarm));
        let starts = !std::mem::replace(&mut state.running, true);
        drop(state);
        if starts {
            self.start();
        }
        self.shared.changed.notify_all();
        Set { shared: Arc::downgrade(&self.shared), key }
    }

    fn start(&self) {
        let shared = Arc::clone(&self.shared);
        let spawned = thread::Builder::new().name("tinystore-timer".to_owned()).spawn(move || shared.run());
        match spawned {
            Ok(handle) => {
                let mut thread = self.thread.lock().unwrap_or_else(PoisonError::into_inner);
                // the thread that ended idle before this one is joined here
                if let Some(ended) = thread.replace(handle) {
                    let _ = ended.join();
                }
            }
            // With no thread, the alarms ring now: a waiter gives up early
            // rather than wait for a time nothing would ring.
            Err(_) => {
                let mut state = self.shared.lock();
                state.running = false;
                let alarms = std::mem::take(&mut state.alarms);
                drop(state);
                alarms.into_values().for_each(|alarm| alarm(Rang::Due));
            }
        }
    }

    /// Rings every alarm set `Stopped`, here, and ends the thread: an alarm
    /// set after rings `Stopped` at once.
    pub(crate) fn stop(&self) {
        let mut state = self.shared.lock();
        state.stopped = true;
        let alarms = std::mem::take(&mut state.alarms);
        drop(state);
        self.shared.changed.notify_all();
        alarms.into_values().for_each(|alarm| alarm(Rang::Stopped));
        let thread = self.thread.lock().unwrap_or_else(PoisonError::into_inner).take();
        // an alarm that drops the store's last handle stops the timer from its own thread
        if let Some(thread) = thread.filter(|thread| thread.thread().id() != thread::current().id()) {
            let _ = thread.join();
        }
    }

    #[cfg(test)]
    fn running(&self) -> bool {
        self.shared.lock().running
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.stop();
    }
}

impl std::fmt::Debug for Timer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Timer({} alarms)", self.shared.lock().alarms.len())
    }
}

impl Set {
    /// Takes the alarm back unrung; false once it rang or is ringing.
    pub(crate) fn cancel(&self) -> bool {
        let Some(shared) = self.shared.upgrade() else { return false };
        let taken = shared.lock().alarms.remove(&self.key);
        taken.is_some()
    }
}

impl Shared {
    fn run(&self) {
        let mut state = self.lock();
        loop {
            if state.stopped {
                return;
            }
            let now = Instant::now();
            match state.alarms.first_key_value().map(|((at, _), _)| *at) {
                Some(at) if at <= now => {
                    let (_, alarm) = state.alarms.pop_first().expect("the first alarm is there");
                    drop(state);
                    alarm(Rang::Due);
                    state = self.lock();
                }
                Some(at) => {
                    state = self.changed.wait_timeout(state, at - now).unwrap_or_else(PoisonError::into_inner).0
                }
                None => {
                    let (woken, slept) =
                        self.changed.wait_timeout(state, RETIRE_AFTER).unwrap_or_else(PoisonError::into_inner);
                    state = woken;
                    if slept.timed_out() && state.alarms.is_empty() {
                        state.running = false;
                        return;
                    }
                }
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn alarms_ring_in_the_order_of_their_times_on_the_timers_thread() {
        let timer = Timer::new();
        let (rang, rung) = mpsc::channel();
        let now = Instant::now();
        for (name, after) in [("third", 60), ("first", 20), ("second", 40)] {
            let rang = rang.clone();
            timer.at(now + Duration::from_millis(after), move |why| {
                rang.send((name, why, thread::current().name().map(str::to_owned))).unwrap();
            });
        }
        let order: Vec<_> = (0..3).map(|_| rung.recv_timeout(Duration::from_secs(10)).unwrap()).collect();
        let timer_thread = Some("tinystore-timer".to_owned());
        assert_eq!(
            order,
            [
                ("first", Rang::Due, timer_thread.clone()),
                ("second", Rang::Due, timer_thread.clone()),
                ("third", Rang::Due, timer_thread)
            ]
        );
        assert!(Instant::now() >= now + Duration::from_millis(60), "none rang before its time");
    }

    #[test]
    fn an_alarm_taken_back_never_rings() {
        let timer = Timer::new();
        let (rang, rung) = mpsc::channel();
        let set = timer.at(Instant::now() + Duration::from_millis(20), move |why| rang.send(why).unwrap());
        assert!(set.cancel());
        assert!(!set.cancel(), "taken back once");
        assert!(rung.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn a_stop_rings_every_alarm_stopped_and_one_set_after_rings_at_once() {
        let timer = Timer::new();
        let (rang, rung) = mpsc::channel();
        let later = rang.clone();
        timer.at(Instant::now() + Duration::from_secs(3600), move |why| rang.send(why).unwrap());
        timer.stop();
        assert_eq!(rung.try_recv().unwrap(), Rang::Stopped, "the alarm rang before the stop returned");
        timer.at(Instant::now() + Duration::from_secs(3600), move |why| later.send(why).unwrap());
        assert_eq!(rung.try_recv().unwrap(), Rang::Stopped);
    }

    #[test]
    fn the_thread_starts_with_an_alarm_and_runs_while_one_is_set() {
        let timer = Timer::new();
        assert!(!timer.running(), "no alarm, no thread");
        let (rang, rung) = mpsc::channel();
        timer.at(Instant::now(), move |why| rang.send(why).unwrap());
        assert_eq!(rung.recv_timeout(Duration::from_secs(10)).unwrap(), Rang::Due);
        assert!(timer.running(), "it sleeps a minute before it ends");
        timer.stop();
    }
}
