//! The threads that run Rust's handlers. A subscription's calls come one at a
//! time and in order, on whichever of the store's delivery threads is free:
//! a handler that takes long holds one thread and slows no other
//! subscription, and one that falls behind its log misses messages rather
//! than holding the publisher.
//!
//! A subscription is idle, queued for a thread, running on one, or stopped:
//!
//! ```text
//! idle      → a message comes      → queued
//! queued    → a thread takes it    → running, up to 64 calls
//! running   → a message comes      → running, and queued again once done
//! running   → its log holds none   → idle
//! any       → stop                 → stopped, once the running call ends
//! ```

use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, Weak};
use std::thread::{self, JoinHandle, ThreadId};

use super::hub::{Hub, LogKey};
use super::log::{Log, Released};
use crate::sync::lock;
use crate::{Error, Result};

/// The threads a store runs Rust's handlers on, started with its first
/// subscription.
const THREADS: usize = 4;

/// The calls a subscription makes on a thread before the next waiting one's
/// turn.
const BATCH: usize = 64;

/// What a subscription's handler is called with.
pub(crate) enum Delivery {
    Missed(u64),
    Message(Arc<Released>),
}

pub(crate) type Run = Box<dyn FnMut(Delivery) + Send>;

#[derive(Default)]
pub(crate) struct Deliverer {
    pool: Arc<Pool>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    /// Every subscription not stopped, by its number in its log: what keeps
    /// one whose handle was dropped listening, until `stop` or the close.
    listening: Mutex<HashMap<u64, Arc<Subscriber>>>,
}

#[derive(Default)]
struct Pool {
    ready: Mutex<Ready>,
    changed: Condvar,
}

#[derive(Default)]
struct Ready {
    queue: VecDeque<Arc<Subscriber>>,
    stopped: bool,
}

/// A subscription whose handler runs on the delivery threads.
pub(crate) struct Subscriber {
    key: LogKey,
    /// Its log and its number in it, once it joined.
    place: OnceLock<(Arc<Log>, u64)>,
    hub: Weak<Hub>,
    pool: Weak<Pool>,
    run: Mutex<Run>,
    turn: Mutex<Turn>,
    /// Set as `stop` begins, before it waits out the running batch, so that
    /// no call of that batch starts after it.
    stopping: AtomicBool,
    changed: Condvar,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Turn {
    Idle,
    Queued,
    Running { on: ThreadId, again: bool },
    Stopped,
}

impl Deliverer {
    /// Starts a subscription of `key` whose handler `run` is called on the
    /// delivery threads with what its log holds for it.
    pub(crate) fn subscribe(&self, hub: &Arc<Hub>, key: LogKey, run: Run) -> Result<Arc<Subscriber>> {
        self.start()?;
        let subscriber = Arc::new(Subscriber {
            key,
            place: OnceLock::new(),
            hub: Arc::downgrade(hub),
            pool: Arc::downgrade(&self.pool),
            run: Mutex::new(run),
            turn: Mutex::new(Turn::Idle),
            stopping: AtomicBool::new(false),
            changed: Condvar::new(),
        });
        let woken = Arc::downgrade(&subscriber);
        let wake = Arc::new(move || {
            if let Some(subscriber) = woken.upgrade() {
                subscriber.wake();
            }
        });
        let place = hub.join(&subscriber.key, wake)?;
        let (log, reader) = subscriber.place.get_or_init(|| place);
        lock(&self.listening).insert(*reader, Arc::clone(&subscriber));
        // A close that emptied the map before the insert is met here.
        if lock(&self.pool.ready).stopped {
            self.forget(*reader);
        }
        // A publish between joining and the place being set found no place to
        // deliver from; it is delivered now.
        if log.holds_for(*reader) {
            subscriber.wake();
        }
        Ok(subscriber)
    }

    fn start(&self) -> Result<()> {
        let mut threads = lock(&self.threads);
        if !threads.is_empty() {
            return Ok(());
        }
        for _ in 0..THREADS {
            let pool = Arc::clone(&self.pool);
            let thread = thread::Builder::new()
                .name("tinystore-channels".to_owned())
                .spawn(move || pool.run())
                .map_err(|error| Error::io("channels: a delivery thread", error))?;
            threads.push(thread);
        }
        Ok(())
    }

    /// Stops the threads once their running calls end; a call that closes
    /// the store from a handler ends its own thread on return.
    pub(crate) fn close(&self) {
        {
            let mut ready = lock(&self.pool.ready);
            ready.stopped = true;
            ready.queue.clear();
        }
        self.pool.changed.notify_all();
        let current = thread::current().id();
        let threads = std::mem::take(&mut *lock(&self.threads));
        for thread in threads.into_iter().filter(|thread| thread.thread().id() != current) {
            let _ = thread.join();
        }
        // Dropped outside the lock: a handler may hold what takes it again.
        let listening = std::mem::take(&mut *lock(&self.listening));
        drop(listening);
    }

    /// Lets go of a subscription that stopped.
    fn forget(&self, reader: u64) {
        let subscriber = lock(&self.listening).remove(&reader);
        drop(subscriber);
    }
}

impl Pool {
    fn run(&self) {
        loop {
            let next = {
                let ready = lock(&self.ready);
                let mut ready = self
                    .changed
                    .wait_while(ready, |ready| ready.queue.is_empty() && !ready.stopped)
                    .unwrap_or_else(PoisonError::into_inner);
                if ready.stopped {
                    return;
                }
                ready.queue.pop_front()
            };
            if let Some(subscriber) = next {
                subscriber.deliver();
            }
        }
    }

    fn push(&self, subscriber: Arc<Subscriber>) {
        let mut ready = lock(&self.ready);
        if !ready.stopped {
            ready.queue.push_back(subscriber);
            drop(ready);
            self.changed.notify_one();
        }
    }
}

impl Subscriber {
    /// Its log holds something for it: queued for a thread, or run again
    /// once its running calls end.
    fn wake(self: &Arc<Self>) {
        let mut turn = lock(&self.turn);
        match *turn {
            Turn::Idle => {
                *turn = Turn::Queued;
                drop(turn);
                if let Some(pool) = self.pool.upgrade() {
                    pool.push(Arc::clone(self));
                }
            }
            Turn::Running { on, .. } => *turn = Turn::Running { on, again: true },
            Turn::Queued | Turn::Stopped => {}
        }
    }

    /// Calls the handler with what its log holds, up to a batch, then queues
    /// itself again behind the others when more waits.
    fn deliver(self: &Arc<Self>) {
        {
            let mut turn = lock(&self.turn);
            if *turn != Turn::Queued {
                return;
            }
            *turn = Turn::Running { on: thread::current().id(), again: false };
        }
        let Some((log, reader)) = self.place.get() else {
            // Woken before its place was set: `subscribe` wakes it again.
            *lock(&self.turn) = Turn::Idle;
            return;
        };
        let taken = log.take(*reader, BATCH);
        let missed = (taken.missed > 0).then_some(Delivery::Missed(taken.missed));
        for delivery in missed.into_iter().chain(taken.messages.into_iter().map(Delivery::Message)) {
            if self.stopped() {
                break;
            }
            let mut run = lock(&self.run);
            // A handler's panic is its own: the typed handler reports it, and
            // the next message follows.
            let _ = catch_unwind(AssertUnwindSafe(|| run(delivery)));
        }
        self.finish_turn(log, *reader);
    }

    fn finish_turn(self: &Arc<Self>, log: &Log, reader: u64) {
        let again = {
            let mut turn = lock(&self.turn);
            match *turn {
                Turn::Running { again, .. } if again || log.holds_for(reader) => {
                    *turn = Turn::Queued;
                    true
                }
                Turn::Running { .. } => {
                    *turn = Turn::Idle;
                    false
                }
                _ => false,
            }
        };
        self.changed.notify_all();
        if again && let Some(pool) = self.pool.upgrade() {
            pool.push(Arc::clone(self));
        }
    }

    fn stopped(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// No call starts after it, and it returns once the running one ended;
    /// called from that call's own thread, at once.
    pub(crate) fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let current = thread::current().id();
        let turn = lock(&self.turn);
        let mut turn = self
            .changed
            .wait_while(turn, |turn| matches!(*turn, Turn::Running { on, .. } if on != current))
            .unwrap_or_else(PoisonError::into_inner);
        if *turn == Turn::Stopped {
            return;
        }
        *turn = Turn::Stopped;
        drop(turn);
        self.changed.notify_all();
        if let (Some(hub), Some((_, reader))) = (self.hub.upgrade(), self.place.get()) {
            hub.leave(&self.key, *reader);
            hub.deliverer.forget(*reader);
        }
    }
}
