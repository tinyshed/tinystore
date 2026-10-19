//! The store's locks: one state of every key held or waited for, the epoch
//! their fences start from, and the thread that wakes when something of them
//! comes due.
//!
//! One process holds a store's directory, so every holder and every waiter is
//! known here, and nothing about a lock is written but the epoch. A lease
//! costs no commit; a store that opens again holds none, since the threads
//! that held them ended with its process and a client learns it at its next
//! renewal.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::lease::{Answer, Ask, LeaseId, Reply};
use super::state::{Effects, Mint, State};
use crate::engine::{Engine, Host};
use crate::kv::engine::Kv;
use crate::sqlite::sql_error;
use crate::sync::lock;
use crate::{Clock, Error, Result, Store, unix_millis};

/// The epoch each opening of the locks starts its fences from: the time in
/// milliseconds, or one past the last epoch when the clock is behind it, so
/// that a store's fences grow across its restarts whatever its clock does.
const NEXT_EPOCH: &str = "insert into _tinystore_kv_meta (name, value) values ('epoch', ?1) \
                          on conflict (name) do update set value = max(?1, value + 1) returning value";

/// The longest the timer sleeps without reading the clock again, so that a
/// clock that jumps is met.
const LONGEST_SLEEP: Duration = Duration::from_secs(60);

pub(crate) struct Locks {
    inner: Arc<Inner>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

struct Inner {
    probe: Arc<crate::probe::Probe>,
    mint: Mint,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
    timer: Timer,
}

impl Locks {
    /// The store's locks, opened by the first handle.
    pub(crate) fn of(store: &Store) -> Result<Arc<Locks>> {
        // kv opens through the store's registry, which holds its lock while an
        // engine opens: asking for it from inside would wait for itself.
        let kv = Kv::of(store)?;
        store.engine(|store| Locks::open(store, &kv))
    }

    fn open(store: &Store, kv: &Kv) -> Result<Arc<Locks>> {
        let clock = store.clock();
        let epoch = next_epoch(kv, unix_millis(clock.now()))?;
        let mint = Mint { epoch, count: AtomicU64::new(0) };
        let probe = Arc::clone(store.probe());
        let inner = Arc::new(Inner { probe, mint, clock, state: Mutex::default(), timer: Timer::default() });
        let timed = Arc::clone(&inner);
        let thread = thread::Builder::new()
            .name("tinystore-locks".to_owned())
            .spawn(move || timed.run_timer())
            .map_err(|error| Error::io("locks: the timer's thread", error))?;
        Ok(Arc::new(Locks { inner, thread: Mutex::new(Some(thread)) }))
    }

    /// Grants the key, puts the ask in line, or answers busy through `reply`;
    /// says the waiter's id when it waits.
    pub(crate) fn ask(&self, ask: Ask, reply: Reply) -> Option<u64> {
        let reply = match self.inner.probe.start() {
            None => reply,
            Some(started) => {
                let (probe, name) = (Arc::clone(&self.inner.probe), Arc::clone(ask.place.name()));
                Box::new(move |answer: Result<Answer>| {
                    let labels = [("lock", &*name)];
                    probe.time("tinystore_locks_wait_ms", &labels, started.elapsed());
                    if matches!(answer, Ok(Answer::Busy)) {
                        probe.count("tinystore_locks_busy_total", &labels, 1.0);
                    }
                    reply(answer);
                }) as Reply
            }
        };
        self.inner.change(|state, now, mint, effects| state.ask(ask, reply, now, mint, effects))
    }

    /// Lets go of a lease and gives its key on; says whether it was held.
    pub(crate) fn release(&self, lease: &LeaseId) -> bool {
        self.inner.change(|state, now, mint, effects| state.release(lease, now, mint, effects))
    }

    /// Renews a lease held through a connection for `ttl`, and says its new
    /// deadline in unix milliseconds.
    #[cfg(feature = "wire")]
    pub(crate) fn renew(&self, lease: &LeaseId, ttl: Duration) -> Result<i64> {
        self.inner.change(|state, now, mint, effects| state.renew(lease, ttl, now, mint, effects))
    }

    /// Takes a waiter out of its line; its reply is dropped unanswered.
    #[cfg(feature = "wire")]
    pub(crate) fn cancel(&self, waiter: u64) {
        self.inner.change(|state, _, _, _| state.cancel(waiter));
    }

    #[cfg(test)]
    pub(crate) fn epoch(&self) -> u64 {
        self.inner.mint.epoch
    }

    /// How many wait in every line.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        lock(&self.inner.state).waiting()
    }
}

impl Inner {
    /// Changes the state at the store's time now, then runs what the change
    /// owes the holders, outside the lock.
    fn change<T>(&self, work: impl FnOnce(&mut State, i64, &Mint, &mut Effects) -> T) -> T {
        let mut effects = Effects::default();
        let now = unix_millis(self.clock.now());
        let value = work(&mut lock(&self.state), now, &self.mint, &mut effects);
        if effects.sooner {
            self.timer.wake();
        }
        let lost = effects.lost();
        if lost > 0 {
            self.probe.count("tinystore_locks_lost_total", &[], lost as f64);
        }
        effects.run();
        value
    }

    /// Settles what comes due, then sleeps until the next thing does, until
    /// the locks close.
    fn run_timer(&self) {
        loop {
            let next = lock(&self.state).next_due();
            let now = unix_millis(self.clock.now());
            if next.is_some_and(|at| at <= now) {
                self.change(|state, now, mint, effects| state.tick(now, mint, effects));
                continue;
            }
            let left = next.map(|at| Duration::from_millis(u64::try_from(at - now).unwrap_or(0)));
            if !self.timer.sleep(left.map_or(LONGEST_SLEEP, |left| left.min(LONGEST_SLEEP))) {
                return;
            }
        }
    }
}

impl Engine for Locks {
    /// Ends every lease and answers every waiter `closed`, then stops the
    /// timer.
    fn close(&self) -> Result<()> {
        self.inner.change(|state, _, _, effects| state.close(effects));
        self.inner.timer.stop();
        let handle = lock(&self.thread).take();
        // The last handle of a store dropped by a holder's reply, on the
        // timer's own thread, closes it there; that thread ends on return.
        if let Some(handle) = handle.filter(|handle| handle.thread().id() != thread::current().id()) {
            let _ = handle.join();
        }
        Ok(())
    }

    fn clock_moved(&self) {
        self.inner.timer.wake();
    }
}

fn next_epoch(kv: &Kv, now: i64) -> Result<u64> {
    let epoch = kv.file().write(32, move |tx| {
        let mut next = tx.prepare_cached(NEXT_EPOCH).map_err(|error| sql_error("the locks' epoch", error))?;
        next.query_row([now], |row| row.get::<_, i64>(0)).map_err(|error| sql_error("the locks' epoch", error))
    })?;
    u64::try_from(epoch).map_err(|_| Error::corrupt(format!("kv.db: the locks' epoch is {epoch}, below zero")))
}

/// What the timer's thread sleeps on: woken when something comes due sooner,
/// when the clock is moved, and when the locks close.
#[derive(Default)]
struct Timer {
    waking: Mutex<Waking>,
    changed: Condvar,
}

#[derive(Default)]
struct Waking {
    woken: bool,
    stopped: bool,
}

impl Timer {
    fn wake(&self) {
        lock(&self.waking).woken = true;
        self.changed.notify_all();
    }

    fn stop(&self) {
        lock(&self.waking).stopped = true;
        self.changed.notify_all();
    }

    /// Sleeps `span` or until woken; false once stopped.
    fn sleep(&self, span: Duration) -> bool {
        let waking = lock(&self.waking);
        let (mut waking, _) = self
            .changed
            .wait_timeout_while(waking, span, |waking| !waking.woken && !waking.stopped)
            .unwrap_or_else(PoisonError::into_inner);
        waking.woken = false;
        !waking.stopped
    }
}
