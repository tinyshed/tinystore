//! The store's channels: the log of each branch someone listens to, found by
//! its channel, its branch and whether it takes the branches below, and the
//! memory every message is held in.
//!
//! A publish puts its message in the log of its own branch and in those of
//! its ancestors listened to `recursive`; a branch nobody listens to keeps
//! nothing. Under the writer of a commit this is all that happens: a message
//! put in its logs, and the subscribers it reached marked ready.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::deliver::Deliverer;
use super::log::{Log, Released, Wake};
use crate::engine::{Engine, Host};
use crate::memory::Reservation;
use crate::{Error, Memory, Result, Store};

/// The bytes every channel's messages may hold together, of the store's
/// memory: past them the oldest messages of the longest log are dropped, so
/// that a publisher never waits for a slow subscriber, nor fails for one.
const MOST_HELD: u64 = 64 << 20;

/// The subscriptions a store holds at once.
pub(super) const MOST_SUBSCRIPTIONS: usize = 10_000;

/// A log by what it takes: a channel's branch, and whether the branches below
/// it too.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LogKey {
    pub(crate) channel: Arc<str>,
    pub(crate) branch: Vec<String>,
    pub(crate) recursive: bool,
}

pub(crate) struct Hub {
    held: Arc<Memory>,
    store: Arc<Memory>,
    logs: Mutex<Logs>,
    next_reader: AtomicU64,
    closed: AtomicBool,
    pub(super) deliverer: Deliverer,
}

#[derive(Default)]
struct Logs {
    by_key: HashMap<LogKey, Arc<Log>>,
    readers: usize,
}

impl Hub {
    /// The store's channels, opened by the first handle.
    pub(crate) fn of(store: &Store) -> Result<Arc<Hub>> {
        store.engine(|store| {
            Ok(Arc::new(Hub {
                held: Memory::new(MOST_HELD),
                store: Arc::clone(store.memory()),
                logs: Mutex::default(),
                next_reader: AtomicU64::new(0),
                closed: AtomicBool::new(false),
                deliverer: Deliverer::default(),
            }))
        })
    }

    pub(crate) fn closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Puts a message in the logs of its branch and of the ancestors listened
    /// to below, then wakes the subscribers it reached.
    pub(crate) fn publish(&self, channel: &Arc<str>, branch: Vec<String>, value: Vec<u8>) -> Result<()> {
        if self.closed() {
            return Err(Error::closed(format!("channel {channel}")));
        }
        let logs = self.logs_reached(channel, &branch);
        if logs.is_empty() {
            return Ok(());
        }
        let bytes = value.len() + branch.iter().map(String::len).sum::<usize>();
        let reserved = self.reserve(bytes as u64).map_err(|error| error.within(format!("channel {channel}")))?;
        let message = Arc::new(Released { branch, value, _reserved: reserved });
        let wakes: Vec<Wake> = logs.iter().flat_map(|log| log.push(Arc::clone(&message))).collect();
        for wake in wakes {
            wake();
        }
        Ok(())
    }

    /// The logs a message of `branch` goes to: its own, and the recursive
    /// ones of its ancestors and itself.
    fn logs_reached(&self, channel: &Arc<str>, branch: &[String]) -> Vec<Arc<Log>> {
        let logs = self.lock();
        let exact = LogKey { channel: Arc::clone(channel), branch: branch.to_vec(), recursive: false };
        let ancestors = (0..=branch.len()).map(|depth| LogKey {
            channel: Arc::clone(channel),
            branch: branch[..depth].to_vec(),
            recursive: true,
        });
        std::iter::once(exact).chain(ancestors).filter_map(|key| logs.by_key.get(&key).cloned()).collect()
    }

    /// Reserves a message's bytes of the channels' share and of the store's
    /// memory, dropping the oldest messages of the longest log while they
    /// are short.
    fn reserve(&self, bytes: u64) -> Result<[Reservation; 2]> {
        loop {
            let held = self.held.try_reserve(bytes, "channels' messages");
            let store = held.and_then(|held| Ok([held, self.store.try_reserve(bytes, "channels' messages")?]));
            match store {
                Ok(reserved) => return Ok(reserved),
                Err(error) if self.shed() == 0 => return Err(error),
                Err(_) => {}
            }
        }
    }

    /// Drops the oldest message of the log that holds the most; says the
    /// bytes it let go of.
    fn shed(&self) -> usize {
        let longest = self.lock().by_key.values().max_by_key(|log| log.bytes()).cloned();
        longest.map_or(0, |log| log.shed())
    }

    /// Takes a subscriber into the log of `key`, made when nobody listened
    /// there yet; gives back its log and its number.
    pub(crate) fn join(&self, key: &LogKey, wake: Wake) -> Result<(Arc<Log>, u64)> {
        let mut logs = self.lock();
        if self.closed() {
            return Err(Error::closed(format!("channel {}", key.channel)));
        }
        if logs.readers >= MOST_SUBSCRIPTIONS {
            return Err(Error::limit(format!("{MOST_SUBSCRIPTIONS} subscriptions of the store's channels")));
        }
        let log = Arc::clone(logs.by_key.entry(key.clone()).or_insert_with(|| Arc::new(Log::new())));
        let reader = self.next_reader.fetch_add(1, Ordering::Relaxed);
        log.join(reader, wake);
        logs.readers += 1;
        Ok((log, reader))
    }

    /// Lets a subscriber go, and its log once nobody is left in it.
    pub(crate) fn leave(&self, key: &LogKey, reader: u64) {
        let mut logs = self.lock();
        let Some(log) = logs.by_key.get(key).cloned() else { return };
        if !log.leave(reader) {
            logs.by_key.remove(key);
        }
        logs.readers = logs.readers.saturating_sub(1);
    }

    fn lock(&self) -> MutexGuard<'_, Logs> {
        self.logs.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Engine for Hub {
    /// Stops every subscription and drops every message; a handler running
    /// finishes, and an `each` waiting ends.
    fn close(&self) -> Result<()> {
        self.closed.store(true, Ordering::Release);
        self.deliverer.close();
        let logs: Vec<Arc<Log>> = self.lock().by_key.drain().map(|(_, log)| log).collect();
        for log in logs {
            log.end();
        }
        Ok(())
    }
}
