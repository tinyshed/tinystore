//! A branch's log: the messages its subscribers have not all read, and each
//! subscriber's place in it.
//!
//! A message is kept once however many logs hold it; a log holds it by
//! reference until every subscriber of the log has read it, so a log whose
//! subscribers keep up holds nothing. One that falls behind the log's bounds
//! loses what it skipped and is told how many, before its next message:
//!
//! ```text
//! places: a at 7, b at 3; the log holds 3..=9, its bound is 6 messages
//! a message 10 comes        → the log drops 3 and 4; b is at 5, 2 missed
//! b reads                   → missed 2, then 5, 6 … as it asks
//! ```

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::memory::Reservation;

/// What a message costs a batch on the wire beyond its bytes: its fields'
/// heads, and each owner's.
const OVERHEAD: usize = 16;
const OWNER_OVERHEAD: usize = 5;

/// The most messages, and bytes of them, a branch's log keeps unread.
pub(super) const MOST_MESSAGES: usize = 1024;
pub(super) const MOST_BYTES: usize = 4 << 20;

/// A message once published: its branch and its value as JSON, held by every
/// log that keeps it, its bytes reserved of the store's memory until the last
/// log lets it go.
pub(crate) struct Released {
    pub(crate) branch: Vec<String>,
    pub(crate) value: Vec<u8>,
    pub(super) _reserved: [Reservation; 2],
}

impl Released {
    pub(super) fn bytes(&self) -> usize {
        self.value.len() + self.branch.iter().map(String::len).sum::<usize>()
    }
}

/// What tells a subscriber that its log holds something for it: it must
/// return quickly and not call into the log.
pub(crate) type Wake = Arc<dyn Fn() + Send + Sync>;

/// What a subscriber takes from its log: how many it missed since it last
/// took, then messages in order.
#[derive(Default)]
pub(crate) struct Taken {
    pub(crate) missed: u64,
    pub(crate) messages: Vec<Arc<Released>>,
}

pub(crate) struct Log {
    state: Mutex<State>,
    /// Where what its subscribers take and miss is counted, by its channel.
    measured: Option<(Arc<crate::probe::Probe>, Arc<str>)>,
}

#[derive(Default)]
struct State {
    /// The number of the first message held; the next is `first + len`.
    first: u64,
    messages: VecDeque<Arc<Released>>,
    bytes: usize,
    places: HashMap<u64, Place>,
}

struct Place {
    next: u64,
    missed: u64,
    wake: Wake,
    /// Woken since it last took: a second wake would do nothing more.
    woken: bool,
}

impl Log {
    pub(super) fn new() -> Log {
        Log { state: Mutex::default(), measured: None }
    }

    /// The log, what its subscribers take and miss counted in the store's own
    /// metrics under `channel`.
    pub(super) fn measured(self, probe: Arc<crate::probe::Probe>, channel: Arc<str>) -> Log {
        Log { measured: Some((probe, channel)), ..self }
    }

    /// Takes a subscriber in, at the end of the log: it reads what comes from
    /// now on.
    pub(super) fn join(&self, reader: u64, wake: Wake) {
        let mut state = self.lock();
        let next = state.first + state.messages.len() as u64;
        state.places.insert(reader, Place { next, missed: 0, wake, woken: false });
    }

    /// Lets a subscriber go; says whether any are left.
    pub(super) fn leave(&self, reader: u64) -> bool {
        let mut state = self.lock();
        state.places.remove(&reader);
        state.drop_read();
        !state.places.is_empty()
    }

    /// Puts a message at the end of the log, drops what is past its bounds,
    /// and gives back the wakes of the subscribers not woken yet.
    pub(super) fn push(&self, message: Arc<Released>) -> Vec<Wake> {
        let mut state = self.lock();
        state.bytes += message.bytes();
        state.messages.push_back(message);
        while state.messages.len() > MOST_MESSAGES || state.bytes > MOST_BYTES {
            state.drop_first();
        }
        let wakes = state.places.values_mut().filter(|place| !place.woken);
        wakes
            .map(|place| {
                place.woken = true;
                Arc::clone(&place.wake)
            })
            .collect()
    }

    /// Drops the oldest message unread, to make room in the store's memory;
    /// says the bytes it let go of, nothing when it held none.
    pub(super) fn shed(&self) -> usize {
        let mut state = self.lock();
        let bytes = state.messages.front().map_or(0, |message| message.bytes());
        state.drop_first();
        bytes
    }

    /// Drops every message and wakes every subscriber: the store is closing,
    /// and what waits for the log finds it closed.
    pub(super) fn end(&self) {
        let wakes: Vec<Wake> = {
            let mut state = self.lock();
            state.messages.clear();
            state.bytes = 0;
            state.places.values().map(|place| Arc::clone(&place.wake)).collect()
        };
        for wake in wakes {
            wake();
        }
    }

    /// The bytes of the messages the log holds.
    pub(super) fn bytes(&self) -> usize {
        self.lock().bytes
    }

    /// Takes up to `most` messages for a subscriber, with what it missed.
    pub(super) fn take(&self, reader: u64, most: usize) -> Taken {
        self.take_within(reader, most, usize::MAX).0
    }

    /// Takes messages for a subscriber while their bytes, and what each costs
    /// a batch on the wire, stay within `most_bytes`, and `most` of them at
    /// most; says too whether the next message waits because it did not fit.
    pub(crate) fn take_within(&self, reader: u64, most: usize, most_bytes: usize) -> (Taken, bool) {
        let mut state = self.lock();
        let (first, end) = (state.first, state.first + state.messages.len() as u64);
        let at = |seq: u64| usize::try_from(seq - first).unwrap_or(usize::MAX);
        let Some(next) = state.places.get(&reader).map(|place| place.next.max(first)) else {
            return (Taken::default(), false);
        };
        let (mut to, mut bytes) = (next, 0usize);
        while to < end && ((to - next) as usize) < most {
            let message = &state.messages[at(to)];
            let size = message.bytes() + OVERHEAD + OWNER_OVERHEAD * message.branch.len();
            if bytes + size > most_bytes {
                break;
            }
            bytes += size;
            to += 1;
        }
        let waits = to < end && ((to - next) as usize) < most;
        let messages = state.messages.range(at(next)..at(to)).cloned().collect();
        let Some(place) = state.places.get_mut(&reader) else { return (Taken::default(), false) };
        place.next = to;
        place.woken = false;
        let missed = std::mem::take(&mut place.missed);
        state.drop_read();
        drop(state);
        let taken = Taken { missed, messages };
        if let (Some((probe, channel)), false) = (&self.measured, taken.messages.is_empty() && taken.missed == 0) {
            let labels = [("channel", &**channel)];
            probe.count("tinystore_channels_delivered_total", &labels, taken.messages.len() as f64);
            if taken.missed > 0 {
                probe.count("tinystore_channels_missed_total", &labels, taken.missed as f64);
            }
        }
        (taken, waits)
    }

    /// Counts a subscriber's next message missed, as though the log had let
    /// it go: one larger than its connection takes.
    #[cfg(feature = "wire")]
    pub(crate) fn skip(&self, reader: u64) {
        let mut state = self.lock();
        let (first, end) = (state.first, state.first + state.messages.len() as u64);
        let Some(place) = state.places.get_mut(&reader) else { return };
        let next = place.next.max(first);
        if next < end {
            place.next = next + 1;
            place.missed += 1;
        }
        state.drop_read();
    }

    /// Whether a subscriber has something to take.
    pub(crate) fn holds_for(&self, reader: u64) -> bool {
        let state = self.lock();
        let end = state.first + state.messages.len() as u64;
        state.places.get(&reader).is_some_and(|place| place.missed > 0 || place.next < end)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl State {
    /// Drops the oldest message; every subscriber that had not read it
    /// missed it.
    fn drop_first(&mut self) {
        let Some(message) = self.messages.pop_front() else { return };
        self.bytes -= message.bytes();
        let dropped = self.first;
        self.first += 1;
        for place in self.places.values_mut().filter(|place| place.next <= dropped) {
            place.missed += 1;
            place.next = self.first;
        }
    }

    /// Drops the messages every subscriber has read.
    fn drop_read(&mut self) {
        let read = self.places.values().map(|place| place.next).min();
        let read = read.unwrap_or(self.first + self.messages.len() as u64);
        while self.first < read {
            let Some(message) = self.messages.pop_front() else { break };
            self.bytes -= message.bytes();
            self.first += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::Memory;

    fn message(text: &str) -> Arc<Released> {
        let memory = Memory::new(0);
        Arc::new(Released {
            branch: Vec::new(),
            value: text.as_bytes().to_vec(),
            _reserved: [memory.nothing(), memory.nothing()],
        })
    }

    fn texts(taken: &Taken) -> Vec<String> {
        taken.messages.iter().map(|message| String::from_utf8(message.value.clone()).unwrap()).collect()
    }

    // contract: CHAN-02
    #[test]
    fn a_log_holds_what_its_subscribers_have_not_all_read_and_says_what_one_missed() {
        let log = Log::new();
        let woken = Arc::new(AtomicUsize::new(0));
        let counting = Arc::clone(&woken);
        log.join(
            1,
            Arc::new(move || {
                counting.fetch_add(1, Ordering::SeqCst);
            }),
        );
        log.join(2, Arc::new(|| {}));
        assert_eq!(log.push(message("a")).len(), 2);
        assert!(log.push(message("b")).is_empty(), "woken once until they take");
        assert_eq!(texts(&log.take(1, 10)), ["a", "b"]);
        assert_eq!(log.bytes(), 2, "the second has not read them");
        assert_eq!(texts(&log.take(2, 1)), ["a"]);
        assert_eq!(log.bytes(), 1, "a read by both is let go");
        for _ in 0..MOST_MESSAGES + 3 {
            for wake in log.push(message("x")) {
                wake();
            }
        }
        let behind = log.take(2, usize::MAX);
        assert_eq!(behind.missed, 4, "b and the three oldest x went past the bound");
        assert_eq!(behind.messages.len(), MOST_MESSAGES);
        assert!(!log.holds_for(2));
        assert!(log.leave(2), "the first is still there");
        assert!(!log.leave(1));
        assert_eq!(log.bytes(), 0, "nobody left to read anything");
        assert_eq!(woken.load(Ordering::SeqCst), 1, "the first woken once more after it took, until it takes again");
    }
}
