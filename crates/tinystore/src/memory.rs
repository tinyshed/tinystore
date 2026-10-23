use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::{Error, Result};

/// What a `Limit` of the store's memory names in its `limit` fact when one
/// call passes the whole budget. Its `wanted` is what the call would have
/// held, and its `bound` the budget.
pub(crate) const STORE_MEMORY: &str = "store memory";
/// The `limit` fact of calls that together pass what is free now: `wanted`
/// is what every call would have held.
pub(crate) const STORE_MEMORY_NOW: &str = "store memory, now";

/// The bytes a store's work may hold at once.
///
/// Work reserves before it allocates and gives the bytes back by dropping its
/// reservation. A request past the limit is a `Limit` error naming what asked:
/// the caller gets a refusal, never a quietly smaller answer. The budget counts
/// what work reserves; it is not a ceiling on the process's RSS.
/// How long a call waits for other work to give the store's memory back
/// before it fails `limit`.
#[cfg(any(feature = "blobs", feature = "records", feature = "metrics"))]
pub(crate) const PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

pub struct Memory {
    limit: u64,
    held: Mutex<Held>,
    freed: Condvar,
}

/// What work holds, and the reservations that wait for room in the order
/// they came. A reservation takes its bytes only while they leave the room
/// every waiter before it waits for, so that small ones that keep coming
/// never pass a large one: the large one is served once what was held before
/// it comes back.
#[derive(Default)]
struct Held {
    used: u64,
    /// Each waiter's ticket and the bytes it waits for, the first come first.
    waiting: VecDeque<(u64, u64)>,
    next_ticket: u64,
}

impl Held {
    /// The bytes the waiters before `ticket` wait for; every waiter's for a
    /// reservation that does not wait.
    fn ahead(&self, ticket: Option<u64>) -> u64 {
        let before = |(waiter, _): &&(u64, u64)| ticket.is_none_or(|ticket| *waiter < ticket);
        self.waiting.iter().take_while(before).fold(0, |sum, (_, bytes)| sum.saturating_add(*bytes))
    }

    fn fits(&self, bytes: u64, ahead: u64, limit: u64) -> bool {
        self.used.saturating_add(ahead).saturating_add(bytes) <= limit
    }

    fn leave(&mut self, ticket: u64) {
        self.waiting.retain(|(waiter, _)| *waiter != ticket);
    }
}

impl Memory {
    pub fn new(limit: u64) -> Arc<Self> {
        Arc::new(Self { limit, held: Mutex::default(), freed: Condvar::new() })
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    pub fn used(&self) -> u64 {
        self.lock().used
    }

    /// Whether the store was given a budget; without one, work reserves
    /// nothing and pays nothing for it.
    pub fn bounded(&self) -> bool {
        self.limit != u64::MAX
    }

    /// Takes `bytes` now, or fails with `Limit`: so does a call that would
    /// take the room a waiting reservation waits for.
    pub fn try_reserve(self: &Arc<Self>, bytes: u64, what: &str) -> Result<Reservation> {
        self.take_now(0, bytes, what)?;
        Ok(self.reservation(bytes))
    }

    /// A reservation of nothing yet, which grows as its work holds more.
    pub fn nothing(self: &Arc<Self>) -> Reservation {
        self.reservation(0)
    }

    /// Takes `bytes` more for work that holds `held` already, now or not at all.
    fn take_now(&self, held: u64, bytes: u64, what: &str) -> Result<()> {
        self.refuse_past_limit(held.saturating_add(bytes), what)?;
        let mut state = self.lock();
        let ahead = state.ahead(None);
        if !state.fits(bytes, ahead, self.limit) {
            return Err(self.exhausted(bytes, &state, ahead, what));
        }
        state.used += bytes;
        Ok(())
    }

    /// Takes `bytes`, waiting up to `patience` for other work to give them
    /// back; waiters are served in the order they came.
    pub fn reserve(self: &Arc<Self>, bytes: u64, what: &str, patience: Duration) -> Result<Reservation> {
        self.refuse_past_limit(bytes, what)?;
        let deadline = Instant::now() + patience;
        let mut state = self.lock();
        let ticket = state.next_ticket;
        state.next_ticket += 1;
        state.waiting.push_back((ticket, bytes));
        loop {
            let ahead = state.ahead(Some(ticket));
            if state.fits(bytes, ahead, self.limit) {
                state.leave(ticket);
                state.used += bytes;
                return Ok(self.reservation(bytes));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                state.leave(ticket);
                let error = self.exhausted(bytes, &state, ahead, what);
                drop(state);
                // the waiters behind it wait for less room now
                self.freed.notify_all();
                return Err(error);
            }
            state = self.freed.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Takes `bytes` at once for work that cannot be refused and holds a
    /// file's writer, as a commit already in its file that its log must keep:
    /// past the limit when there is no room, so that other work waits until
    /// they come back rather than the store believing them free. It never
    /// waits, since every write queued behind the writer would wait with it.
    pub(crate) fn hold(self: &Arc<Self>, bytes: u64) -> Reservation {
        let mut state = self.lock();
        state.used = state.used.saturating_add(bytes);
        drop(state);
        self.reservation(bytes)
    }

    fn refuse_past_limit(&self, bytes: u64, what: &str) -> Result<()> {
        if bytes <= self.limit {
            return Ok(());
        }
        let error =
            Error::limit(format!("{what}: {bytes} bytes is more than the store's memory, {} bytes", self.limit));
        Err(self.named(error, STORE_MEMORY, bytes))
    }

    /// A call that does not fit beside what is held and what the waiters
    /// before it, `ahead`, wait for.
    fn exhausted(&self, bytes: u64, held: &Held, ahead: u64, what: &str) -> Error {
        let used = held.used;
        let waited = match ahead {
            0 => String::new(),
            ahead => format!(", and {ahead} bytes waited for by work that came before it"),
        };
        let error = Error::limit(format!(
            "{what}: {bytes} bytes do not fit in the store's memory, {used} of {} bytes in use{waited}",
            self.limit
        ));
        self.named(error, STORE_MEMORY_NOW, used.saturating_add(ahead).saturating_add(bytes))
    }

    /// A limit of the store's memory with its facts, which every SDK reads.
    fn named(&self, error: Error, limit: &str, wanted: u64) -> Error {
        error.naming("limit", limit).naming("wanted", wanted.to_string()).naming("bound", self.limit.to_string())
    }

    fn reservation(self: &Arc<Self>, bytes: u64) -> Reservation {
        Reservation { memory: Arc::clone(self), bytes }
    }

    fn give_back(&self, bytes: u64) {
        let mut state = self.lock();
        state.used -= bytes;
        drop(state);
        self.freed.notify_all();
    }

    fn lock(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for Memory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Memory({} of {} bytes)", self.used(), self.limit)
    }
}

/// Bytes taken from a store's memory, given back when dropped.
pub struct Reservation {
    memory: Arc<Memory>,
    bytes: u64,
}

impl Reservation {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Takes `bytes` more now, or fails with `Limit` and keeps what it held.
    pub fn grow(&mut self, bytes: u64, what: &str) -> Result<()> {
        self.memory.take_now(self.bytes, bytes, what)?;
        self.bytes += bytes;
        Ok(())
    }

    /// Gives back what the work turned out not to need.
    pub fn shrink(&mut self, to: u64) {
        if to < self.bytes {
            self.memory.give_back(self.bytes - to);
            self.bytes = to;
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.bytes > 0 {
            self.memory.give_back(self.bytes);
        }
    }
}

impl fmt::Debug for Reservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Reservation({} bytes)", self.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;
    use std::thread;

    #[test]
    fn a_reservation_past_what_is_free_is_refused_and_says_who_asked() {
        let memory = Memory::new(100);
        let held = memory.try_reserve(80, "metrics read").unwrap();
        let error = memory.try_reserve(30, "records scan").unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert!(error.what().starts_with("records scan:"), "{error}");
        drop(held);
        assert!(memory.try_reserve(30, "records scan").is_ok());
    }

    // contract: STORE-05
    #[test]
    fn a_request_larger_than_the_whole_budget_fails_without_waiting() {
        let memory = Memory::new(100);
        let started = Instant::now();
        let error = memory.reserve(101, "upload", Duration::from_secs(5)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    // contract: STORE-05
    #[test]
    fn a_waiting_reservation_takes_the_bytes_another_gives_back() {
        let memory = Memory::new(100);
        let held = memory.try_reserve(100, "first").unwrap();
        let waiter = {
            let memory = Arc::clone(&memory);
            thread::spawn(move || memory.reserve(60, "second", Duration::from_secs(10)).map(|r| r.bytes()))
        };
        thread::sleep(Duration::from_millis(20));
        drop(held);
        assert_eq!(waiter.join().unwrap().unwrap(), 60);
        assert_eq!(memory.used(), 0);
    }

    // contract: STORE-05
    #[test]
    fn a_large_waiter_is_served_before_the_small_reservations_that_come_after_it() {
        let memory = Memory::new(100);
        let mut held = std::collections::VecDeque::from([
            memory.try_reserve(30, "a dashboard read").unwrap(),
            memory.try_reserve(30, "a dashboard read").unwrap(),
        ]);
        let large = {
            let memory = Arc::clone(&memory);
            thread::spawn(move || memory.reserve(80, "a records merge", Duration::from_secs(3)).map(|r| r.bytes()))
        };
        thread::sleep(Duration::from_millis(50));
        // small reads keep coming, each taking the room the last gave back
        let mut turn = 0;
        while !large.is_finished() {
            let small = match turn % 3 {
                0 => memory.try_reserve(30, "a dashboard read"),
                1 => memory.reserve(30, "a dashboard read", Duration::from_millis(5)),
                _ => {
                    let mut grown = memory.nothing();
                    grown.grow(30, "a dashboard read").map(|()| grown)
                }
            };
            held.extend(small.ok());
            held.pop_front();
            turn += 1;
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(large.join().unwrap().unwrap(), 80, "the merge waited its turn and got its bytes");
    }

    #[test]
    fn a_reservation_grows_while_the_memory_is_free_and_says_which_bound_it_met() {
        let memory = Memory::new(100);
        let mut rows = memory.nothing();
        rows.grow(60, "rows").unwrap();
        let other = memory.try_reserve(30, "other").unwrap();
        let now = rows.grow(20, "rows").unwrap_err();
        assert_eq!(
            (now.kind(), now.fact("limit"), now.fact("wanted")),
            (ErrorKind::Limit, Some(STORE_MEMORY_NOW), Some("110"))
        );
        assert_eq!(rows.bytes(), 60, "a growth refused keeps what was held");
        drop(other);
        let whole = rows.grow(41, "rows").unwrap_err();
        assert_eq!(
            (whole.fact("limit"), whole.fact("wanted"), whole.fact("bound")),
            (Some(STORE_MEMORY), Some("101"), Some("100"))
        );
        rows.grow(40, "rows").unwrap();
        drop(rows);
        assert_eq!(memory.used(), 0);
    }

    // contract: STORE-05
    #[test]
    fn shrinking_gives_back_the_difference() {
        let memory = Memory::new(100);
        let mut reservation = memory.try_reserve(90, "decode").unwrap();
        reservation.shrink(40);
        assert_eq!(memory.used(), 40);
        drop(reservation);
        assert_eq!(memory.used(), 0);
    }

    // contract: STORE-05
    #[test]
    #[cfg(all(feature = "kv", feature = "replication"))]
    fn a_nodes_commit_holding_its_writer_does_not_wait_for_memory() {
        let dir = tempfile::tempdir().unwrap();
        let node = Some(crate::replication::Node::default());
        let options = crate::Options { node, background: false, memory: Some(1 << 20), ..crate::Options::default() };
        let store = crate::Store::open(dir.path(), options).unwrap();
        let sessions = store.bucket::<String>("sessions").open().unwrap();
        sessions.set("a", &"first".to_owned()).unwrap();
        let reads = store.memory().try_reserve((1 << 20) - 1, "reads").unwrap();
        let started = Instant::now();
        sessions.set("b", &"second".to_owned()).unwrap();
        assert!(started.elapsed() < Duration::from_millis(500), "it waited {:?}", started.elapsed());
        drop(reads);
        assert_eq!(store.memory().used(), 0, "the commit's frames came back");
        store.close().unwrap();
    }

    #[test]
    fn a_hold_that_finds_no_room_takes_its_bytes_past_the_limit_and_gives_them_back() {
        let memory = Memory::new(100);
        let other = memory.try_reserve(80, "other work").unwrap();
        let held = memory.hold(50);
        assert_eq!(memory.used(), 130, "a commit already in its file is never refused");
        assert!(memory.try_reserve(1, "more").is_err(), "other work waits until it comes back");
        drop((held, other));
        assert_eq!(memory.used(), 0);
    }
}
