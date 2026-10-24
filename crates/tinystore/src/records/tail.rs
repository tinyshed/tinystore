//! Tails: the events appended after a tail opened, a batch as they come,
//! from the last megabyte of them the store holds while a tail is open. A
//! reader slower than the writers skips what left the hold, and its next
//! batch says how many: a tail is live and not complete, which `poll` is.
//!
//! ```text
//! held       places 40 41 42 43 44 45 46        a megabyte, the oldest let go
//! a tail at  37                                  37 to 39 left: missed 3, then 40 to 46
//! ```

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::engine::Records;
use super::event::{Event, Kept};
use crate::{Error, ErrorKind, Result};

/// What the events the store holds for its tails weigh at most.
const HELD: usize = 1 << 20;
/// How long a tail walked as an iterator waits at a time for its next batch.
const PATIENCE: Duration = Duration::from_secs(60);

/// Wakes a tail read without a thread of its own: events arrived, or the
/// store closed. It runs on the thread that appended, under no lock of the
/// hold's, and must not wait.
pub(crate) type Waker = Arc<dyn Fn() + Send + Sync>;

/// The events appended lately, held while a tail is open.
#[derive(Default)]
pub(crate) struct Recent {
    state: Mutex<Held>,
    arrived: Condvar,
}

#[derive(Default)]
struct Held {
    /// Each event with its stream's name and its place in the order events
    /// came, the oldest first.
    events: VecDeque<(u64, Arc<str>, Kept)>,
    weight: usize,
    /// The place of the next event to come.
    next: u64,
    tails: usize,
    closed: bool,
    /// What wakes each tail read without a thread, by its number.
    wakers: Vec<(u64, Waker)>,
    /// The number of the last tail opened.
    opened: u64,
}

impl Held {
    fn wakers(&self) -> Vec<Waker> {
        self.wakers.iter().map(|(_, wake)| Arc::clone(wake)).collect()
    }
}

impl Recent {
    fn held(&self) -> MutexGuard<'_, Held> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Holds a write's events while a tail is open, letting the oldest go
    /// past what the hold weighs at most.
    pub(super) fn add<'e>(&self, stream: &Arc<str>, events: impl IntoIterator<Item = &'e Kept>) {
        let mut held = self.held();
        if held.tails == 0 {
            return;
        }
        for event in events {
            let place = held.next;
            held.next += 1;
            held.weight += event.weight();
            held.events.push_back((place, Arc::clone(stream), event.clone()));
        }
        while held.weight > HELD {
            let Some((_, _, gone)) = held.events.pop_front() else { break };
            held.weight -= gone.weight();
        }
        let wakers = held.wakers();
        drop(held);
        self.arrived.notify_all();
        wakers.iter().for_each(|wake| wake());
    }

    /// Ends every tail: the store closes.
    pub(super) fn close(&self) {
        let mut held = self.held();
        held.closed = true;
        let wakers = held.wakers();
        drop(held);
        self.arrived.notify_all();
        wakers.iter().for_each(|wake| wake());
    }
}

/// The events appended after a tail opened that meet its read, a batch as
/// they come; walked as an iterator, it ends when the store closes.
pub struct Tail {
    records: Arc<Records>,
    /// The streams it reads; every one without them.
    streams: Option<Vec<String>>,
    keeps: Box<dyn Fn(&Kept) -> bool + Send + Sync>,
    /// The place of the next event it has not seen.
    next: u64,
    number: u64,
}

impl std::fmt::Debug for Tail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tail").field("streams", &self.streams).field("next", &self.next).finish()
    }
}

/// A batch of a tail: its events, and how many events left the store's hold
/// before the tail read them.
#[derive(Clone, Debug, PartialEq)]
pub struct TailBatch {
    pub events: Vec<Event>,
    pub missed: u64,
}

impl Tail {
    pub(super) fn open(
        records: &Arc<Records>,
        streams: Option<Vec<String>>,
        keeps: Box<dyn Fn(&Kept) -> bool + Send + Sync>,
    ) -> Tail {
        let mut held = records.recent.held();
        held.tails += 1;
        held.opened += 1;
        let (next, number) = (held.next, held.opened);
        drop(held);
        Tail { records: Arc::clone(records), streams, keeps, next, number }
    }

    /// Calls `wake` each time events arrive and when the store closes, for a
    /// reader that waits on no thread of its own: it reads with
    /// `next_within` of no time.
    #[cfg(any(feature = "wire", feature = "async"))]
    pub(crate) fn wake_with(&self, wake: Waker) {
        self.records.recent.held().wakers.push((self.number, wake));
    }

    /// The next batch, waiting up to `wait` for it: none when none came in
    /// that time, and `Closed` once the store has closed.
    pub fn next_within(&mut self, wait: Duration) -> Result<Option<TailBatch>> {
        // A wait past what an instant holds waits without end.
        let deadline = Instant::now().checked_add(wait);
        let recent = &self.records.recent;
        let mut held = recent.held();
        loop {
            if held.closed {
                return Err(Error::closed("records: a tail of a store that closed"));
            }
            if held.next > self.next {
                let batch = self.batch(&held);
                self.next = held.next;
                if !batch.events.is_empty() || batch.missed > 0 {
                    return Ok(Some(batch));
                }
            }
            let Some(deadline) = deadline else {
                held = recent.arrived.wait(held).unwrap_or_else(PoisonError::into_inner);
                continue;
            };
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            held = recent.arrived.wait_timeout(held, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// The events held past the tail's place that meet its read, and how
    /// many events of any stream left the hold before the tail reached them.
    fn batch(&self, held: &Held) -> TailBatch {
        let first = held.events.front().map_or(held.next, |(place, ..)| *place);
        let read = |stream: &str| self.streams.as_ref().is_none_or(|streams| streams.iter().any(|name| name == stream));
        let events = held
            .events
            .iter()
            .filter(|(place, stream, event)| *place >= self.next && read(stream) && (self.keeps)(event));
        TailBatch {
            events: events.map(|(_, stream, event)| event.clone().into_event(stream)).collect(),
            missed: first.saturating_sub(self.next),
        }
    }
}

impl Iterator for Tail {
    type Item = Result<TailBatch>;

    /// The next batch, waited for as long as it takes; none once the store
    /// has closed.
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.next_within(PATIENCE) {
                Ok(Some(batch)) => return Some(Ok(batch)),
                Ok(None) => continue,
                Err(error) if error.kind() == ErrorKind::Closed => return None,
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

impl Drop for Tail {
    fn drop(&mut self) {
        let mut held = self.records.recent.held();
        held.tails -= 1;
        held.wakers.retain(|(number, _)| *number != self.number);
        // with no tail open nothing is held, and appends hold nothing
        if held.tails == 0 {
            held.events.clear();
            held.weight = 0;
        }
    }
}

#[cfg(test)]
#[path = "tail_tests.rs"]
mod tests;
