use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::cancel::{self, Given};
#[cfg(feature = "async")]
use crate::timer::{self, Rang, Timer};
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
#[cfg(any(feature = "blobs", feature = "records", feature = "metrics", feature = "async"))]
pub(crate) const PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

pub struct Memory {
    limit: u64,
    held: Mutex<Held>,
    freed: Condvar,
    /// What ends the patience of a reservation that waits on no thread.
    #[cfg(feature = "async")]
    timer: Arc<Timer>,
}

/// What work holds, and the reservations that wait for room in the order
/// they came. A reservation takes its bytes only while they leave the room
/// every waiter before it waits for, so that small ones that keep coming
/// never pass a large one: the large one is served once what was held before
/// it comes back.
///
/// A waiter is a thread parked in [`Memory::reserve`], which takes its bytes
/// itself once woken, or one that waits on no thread, [`Memory::reserve_then`],
/// which whoever gives bytes back serves, in the same line.
#[derive(Default)]
struct Held {
    used: u64,
    /// Each waiter's ticket and the bytes it waits for, the first come first.
    waiting: VecDeque<(u64, u64)>,
    next_ticket: u64,
    /// The waiters that wait on no thread, by ticket.
    #[cfg(feature = "async")]
    called: std::collections::HashMap<u64, Called>,
}

/// A reservation that waits on no thread: what answers it, and what ends its
/// patience.
#[cfg(feature = "async")]
struct Called {
    done: Box<dyn FnOnce(Result<Reservation>) + Send>,
    what: String,
    alarm: Option<timer::Set>,
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

    /// Puts a waiter for `bytes` at the end of the line, and gives its ticket.
    fn queue(&mut self, bytes: u64) -> u64 {
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        self.waiting.push_back((ticket, bytes));
        ticket
    }

    fn leave(&mut self, ticket: u64) {
        self.waiting.retain(|(waiter, _)| *waiter != ticket);
    }
}

impl Memory {
    pub fn new(limit: u64) -> Arc<Self> {
        #[cfg(feature = "async")]
        return Memory::on(limit, Arc::new(Timer::new()));
        #[cfg(not(feature = "async"))]
        Arc::new(Self { limit, held: Mutex::default(), freed: Condvar::new() })
    }

    /// The store's memory, the patience of its waiters ended on the store's
    /// timer, which stops with the store.
    #[cfg(feature = "async")]
    pub(crate) fn on(limit: u64, timer: Arc<Timer>) -> Arc<Self> {
        Arc::new(Self { limit, held: Mutex::default(), freed: Condvar::new(), timer })
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    pub fn used(&self) -> u64 {
        self.lock().used
    }

    /// The reservations waiting for room, on a thread or not.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.lock().waiting.len()
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
        if bytes == 0 {
            // nothing taken leaves every waiter its room: an empty page, a query matching no series
            return Ok(());
        }
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
    /// back; waiters are served in the order they came, and a call that
    /// takes nothing waits for none of them. A [`crate::CancellationToken`]
    /// around the call does not stop the wait: only reads stop.
    pub fn reserve(self: &Arc<Self>, bytes: u64, what: &str, patience: Duration) -> Result<Reservation> {
        self.wait_for(bytes, what, patience, Waiter::Write)
    }

    /// Takes `bytes` for a read as [`Memory::reserve`] does, but a token of
    /// this thread's call stops its wait: cancelled or past its timeout, the
    /// read leaves the line at once, takes nothing and fails `Cancelled`, as
    /// a read waiting for a reader does (STORE-09).
    #[cfg(any(feature = "records", feature = "metrics"))]
    pub(crate) fn reserve_for_read(
        self: &Arc<Self>,
        bytes: u64,
        what: &str,
        patience: Duration,
    ) -> Result<Reservation> {
        self.wait_for(bytes, what, patience, Waiter::Read)
    }

    fn wait_for(self: &Arc<Self>, bytes: u64, what: &str, patience: Duration, waiter: Waiter) -> Result<Reservation> {
        if bytes == 0 {
            return Ok(self.nothing());
        }
        self.refuse_past_limit(bytes, what)?;
        // a point read an awaited call tries in its poll waits on the pool instead
        if crate::polled::waits_refused() {
            let taken = self.take_now(0, bytes, what);
            return taken.map(|()| self.reservation(bytes)).map_err(|_| crate::polled::refused(|| what.to_owned()));
        }
        // a write's submit reserves through the async API's waiter before it is made
        crate::polled::may_wait(|| what.to_owned())?;
        // given before the lock and dropped after it: a cancel runs the wake under its token's lock
        let _woken = self.woken_by_a_cancel(waiter);
        let deadline = Instant::now() + patience;
        let mut state = self.lock();
        let ticket = state.queue(bytes);
        loop {
            if let Err(cancelled) = waiter.stopped(what) {
                state.leave(ticket);
                self.wake_behind(state);
                return Err(cancelled);
            }
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
                // the waiters behind it wait for less room now
                self.wake_behind(state);
                return Err(error);
            }
            state = self.freed.wait_timeout(state, waiter.sleep(left)).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Has a cancel of a token of this thread's call wake the threads that
    /// wait here, so that a read among them looks at its tokens again; a
    /// write gives none, since no token stops it. The wake takes the lock
    /// first: a read between its look and its sleep holds it, so it sleeps
    /// before the wake comes rather than miss it.
    fn woken_by_a_cancel(self: &Arc<Self>, waiter: Waiter) -> Option<Given> {
        if let Waiter::Write = waiter {
            return None;
        }
        let memory = Arc::clone(self);
        cancel::on_cancel(move || {
            drop(memory.lock());
            memory.freed.notify_all();
        })
    }

    /// Takes `bytes` as [`Memory::reserve`] does, in the same line, but waits
    /// on no thread: `done` is given the reservation once the bytes fit, or
    /// `Limit` once `patience` passed, or `Closed` once the store closed. A
    /// reservation that fits now is answered before this returns, here; one
    /// that waits is answered on the thread that gave the bytes back, or on
    /// the store's timer, so `done` must not block.
    ///
    /// What this returns takes the waiter out of line when it is dropped
    /// unanswered, as an awaited call's future is.
    #[cfg(feature = "async")]
    pub(crate) fn reserve_then(
        self: &Arc<Self>,
        bytes: u64,
        what: &str,
        patience: Duration,
        done: impl FnOnce(Result<Reservation>) + Send + 'static,
    ) -> Waiting {
        let answered = Waiting { memory: std::sync::Weak::new(), ticket: 0 };
        if bytes == 0 {
            done(Ok(self.nothing()));
            return answered;
        }
        if let Err(refused) = self.refuse_past_limit(bytes, what) {
            done(Err(refused));
            return answered;
        }
        let mut state = self.lock();
        let ahead = state.ahead(None);
        if state.fits(bytes, ahead, self.limit) {
            state.used += bytes;
            drop(state);
            done(Ok(self.reservation(bytes)));
            return answered;
        }
        let ticket = state.queue(bytes);
        state.called.insert(ticket, Called { done: Box::new(done), what: what.to_owned(), alarm: None });
        drop(state);
        let memory = Arc::downgrade(self);
        let alarm = self.timer.at(Instant::now() + patience, move |rang| {
            if let Some(memory) = memory.upgrade() {
                memory.give_up(ticket, rang);
            }
        });
        match self.lock().called.get_mut(&ticket) {
            Some(called) => called.alarm = Some(alarm),
            // served already, or rung already by a timer that stopped
            None => drop(alarm.cancel()),
        }
        Waiting { memory: Arc::downgrade(self), ticket }
    }

    /// Ends the wait of the reservation `ticket` that waits on no thread:
    /// `Limit`, its patience out, or `Closed`, its store's timer stopped.
    #[cfg(feature = "async")]
    fn give_up(self: &Arc<Self>, ticket: u64, rang: Rang) {
        let mut state = self.lock();
        let Some(called) = state.called.remove(&ticket) else { return };
        let bytes = state.waiting.iter().find(|(waiter, _)| *waiter == ticket).map_or(0, |(_, bytes)| *bytes);
        let ahead = state.ahead(Some(ticket));
        state.leave(ticket);
        let error = match rang {
            Rang::Due => self.exhausted(bytes, &state, ahead, &called.what),
            Rang::Stopped => Error::closed(format!("{}: the store closed while it waited for memory", called.what)),
        };
        self.wake_behind(state);
        (called.done)(Err(error));
    }

    /// Takes the waiter `ticket` out of line unanswered, as its future is
    /// dropped.
    #[cfg(feature = "async")]
    fn leave_unanswered(self: &Arc<Self>, ticket: u64) {
        let mut state = self.lock();
        let Some(called) = state.called.remove(&ticket) else { return };
        state.leave(ticket);
        self.wake_behind(state);
        if let Some(alarm) = &called.alarm {
            alarm.cancel();
        }
    }

    /// Wakes the waiters, a waiter before them having left or taken its
    /// bytes, or bytes having come back: each thread parked looks again, and
    /// each waiter on no thread whose bytes now fit is served, here.
    fn wake_behind(self: &Arc<Self>, state: MutexGuard<'_, Held>) {
        #[cfg(feature = "async")]
        let mut state = state;
        #[cfg(feature = "async")]
        let served = self.serve(&mut state);
        drop(state);
        self.freed.notify_all();
        #[cfg(feature = "async")]
        for (called, reservation) in served {
            if let Some(alarm) = &called.alarm {
                alarm.cancel();
            }
            (called.done)(Ok(reservation));
        }
    }

    /// Takes the bytes of each waiter on no thread that fits now, in the
    /// order the waiters came: one fits beside what is held and what every
    /// waiter before it waits for, a thread parked or not.
    #[cfg(feature = "async")]
    fn serve(self: &Arc<Self>, state: &mut Held) -> Vec<(Called, Reservation)> {
        let mut served = Vec::new();
        let (mut ahead, mut at) = (0u64, 0);
        while let Some(&(ticket, bytes)) = state.waiting.get(at) {
            if state.called.contains_key(&ticket) && state.fits(bytes, ahead, self.limit) {
                state.waiting.remove(at);
                state.used += bytes;
                let called = state.called.remove(&ticket).expect("a waiter on no thread is called");
                served.push((called, self.reservation(bytes)));
                continue;
            }
            ahead = ahead.saturating_add(bytes);
            at += 1;
        }
        served
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

    fn give_back(self: &Arc<Self>, bytes: u64) {
        let mut state = self.lock();
        state.used -= bytes;
        self.wake_behind(state);
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

/// What waits for memory on a thread: a read, which a token of its call
/// stops, or a write, which runs to its end whatever token is around it.
#[derive(Clone, Copy)]
enum Waiter {
    #[cfg_attr(
        not(any(feature = "records", feature = "metrics")),
        expect(dead_code, reason = "only the reads of records and metrics wait for memory")
    )]
    Read,
    Write,
}

impl Waiter {
    /// `Cancelled` once a read's token has stopped.
    fn stopped(self, what: &str) -> Result<()> {
        match self {
            Waiter::Read => cancel::check(|| what.to_owned()),
            Waiter::Write => Ok(()),
        }
    }

    /// How long the thread sleeps before it looks again: a read wakes at its
    /// tokens' timeout too, which no cancel announces.
    fn sleep(self, left: Duration) -> Duration {
        let timeout = match self {
            Waiter::Read => cancel::deadline(),
            Waiter::Write => None,
        };
        timeout.map_or(left, |at| at.saturating_duration_since(Instant::now()).min(left))
    }
}

/// A reservation [`Memory::reserve_then`] keeps waiting on no thread: dropped
/// before it is answered, it leaves its line, and the waiters behind it wait
/// for less.
#[cfg(feature = "async")]
#[must_use = "dropping it takes the waiter out of line"]
pub(crate) struct Waiting {
    /// Nothing for a reservation answered before it would wait.
    memory: std::sync::Weak<Memory>,
    ticket: u64,
}

#[cfg(feature = "async")]
impl Waiting {
    /// Keeps the waiter in line once this is dropped: a write whose future
    /// was polled waits for its bytes whether or not anyone awaits it.
    pub(crate) fn stay(mut self) {
        self.memory = std::sync::Weak::new();
    }
}

#[cfg(feature = "async")]
impl Drop for Waiting {
    fn drop(&mut self) {
        if let Some(memory) = self.memory.upgrade() {
            memory.leave_unanswered(self.ticket);
        }
    }
}

#[cfg(feature = "async")]
impl fmt::Debug for Waiting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Waiting({})", self.ticket)
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

    // contract: STORE-05
    #[test]
    fn a_reservation_of_nothing_waits_for_no_waiter_before_it() {
        let memory = Memory::new(100);
        let held = memory.try_reserve(60, "a dashboard read").unwrap();
        let large = {
            let memory = Arc::clone(&memory);
            thread::spawn(move || memory.reserve(80, "a records merge", Duration::from_secs(30)).map(|r| r.bytes()))
        };
        while memory.waiting() == 0 {
            thread::yield_now();
        }
        // a read whose page fetched nothing, a metrics query that matched no series
        let empty = memory.try_reserve(0, "records: a read").map(|r| r.bytes());
        let waited = memory.reserve(0, "metrics: a read", Duration::from_secs(1)).map(|r| r.bytes());
        let grown = memory.nothing().grow(0, "its rows");
        drop(held);
        assert_eq!(large.join().unwrap().unwrap(), 80);
        assert_eq!((empty.unwrap(), waited.unwrap()), (0, 0), "nothing taken passes the queue");
        grown.unwrap();
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

    /// Waits until `memory` has `waiters` in line, failing past ten seconds.
    #[cfg(any(feature = "records", feature = "metrics"))]
    fn until_waiting(memory: &Memory, waiters: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while memory.waiting() != waiters {
            assert!(Instant::now() < deadline, "{} waiters, never {waiters}", memory.waiting());
            thread::yield_now();
        }
    }

    // contract: STORE-09
    #[cfg(any(feature = "records", feature = "metrics"))]
    #[test]
    fn a_read_waiting_for_memory_leaves_the_line_once_its_token_stops_and_a_write_waits_on() {
        let memory = Memory::new(100);
        let held = memory.try_reserve(100, "a dashboard read").unwrap();
        let read = |token: crate::CancellationToken| {
            let memory = Arc::clone(&memory);
            let patience = Duration::from_secs(30);
            thread::spawn(move || {
                token.run(|| memory.reserve_for_read(60, "metrics: a read", patience).map(|r| r.bytes()))
            })
        };

        let token = crate::CancellationToken::new();
        let (reading, started) = (read(token.clone()), Instant::now());
        until_waiting(&memory, 1);
        token.cancel();
        let cancelled = reading.join().unwrap().unwrap_err();
        assert_eq!(cancelled.kind(), ErrorKind::Cancelled, "{cancelled}");
        assert!(started.elapsed() < Duration::from_secs(5), "it waited {:?}", started.elapsed());
        assert_eq!(memory.waiting(), 0, "the read left the line");

        // no cancel wakes a read whose token times out: it wakes at the token's deadline itself
        let (timed, started) = (read(crate::CancellationToken::timeout(Duration::from_millis(50))), Instant::now());
        let past = timed.join().unwrap().unwrap_err();
        assert_eq!(past.kind(), ErrorKind::Cancelled, "{past}");
        assert!(past.to_string().contains("past its timeout"), "{past}");
        assert!(started.elapsed() < Duration::from_secs(5), "it waited {:?}", started.elapsed());
        assert_eq!(memory.waiting(), 0);

        // only reads stop: a write under a cancelled token waits on and takes its bytes
        let token = crate::CancellationToken::new();
        let writing = {
            let (memory, token) = (Arc::clone(&memory), token.clone());
            let patience = Duration::from_secs(30);
            thread::spawn(move || token.run(|| memory.reserve(60, "metrics: an ingest", patience).map(|r| r.bytes())))
        };
        until_waiting(&memory, 1);
        token.cancel();
        thread::sleep(Duration::from_millis(50));
        assert!(!writing.is_finished(), "a write is not stopped by its token");
        drop(held);
        assert_eq!(writing.join().unwrap().unwrap(), 60);
        assert_eq!(memory.used(), 0);
    }

    /// A reservation waiting on no thread, whose answer `answers` gets with
    /// its name and the thread it came on.
    #[cfg(feature = "async")]
    type Answers = std::sync::mpsc::Sender<(&'static str, Result<u64>, Option<String>)>;

    #[cfg(feature = "async")]
    fn wait_for(
        memory: &Arc<Memory>,
        name: &'static str,
        bytes: u64,
        patience: Duration,
        answers: &Answers,
    ) -> Waiting {
        let answers = answers.clone();
        memory.reserve_then(bytes, name, patience, move |reserved| {
            let here = thread::current().name().map(str::to_owned);
            // the reservation goes as the answer is noted: a test holds no bytes past it
            answers.send((name, reserved.map(|reservation| reservation.bytes()), here)).unwrap();
        })
    }

    // contract: STORE-05
    // contract: STORE-10
    #[cfg(feature = "async")]
    #[test]
    fn waiters_on_no_thread_are_served_in_the_order_they_came_by_whoever_gives_bytes_back() {
        let memory = Memory::new(100);
        let (first, second) = (memory.try_reserve(60, "a read").unwrap(), memory.try_reserve(40, "a read").unwrap());
        let (answers, answered) = std::sync::mpsc::channel();
        let patience = Duration::from_secs(30);
        let waiting = [
            wait_for(&memory, "a merge", 60, patience, &answers),
            wait_for(&memory, "an append", 30, patience, &answers),
            wait_for(&memory, "a page", 10, patience, &answers),
        ];
        assert!(answered.try_recv().is_err(), "none fits, and none of them parked this thread");

        // 40 free: the page's 10 would fit, but not past the merge before it
        drop(second);
        assert!(answered.try_recv().is_err(), "a small waiter does not pass a large one");
        let giving = thread::Builder::new().name("a-reader".to_owned()).spawn(move || drop(first)).unwrap();
        giving.join().unwrap();
        let served: Vec<_> = answered.try_iter().collect();
        let names: Vec<_> = served.iter().map(|(name, _, _)| *name).collect();
        assert_eq!(names, ["a merge", "an append", "a page"]);
        assert!(served.iter().all(|(_, reserved, _)| reserved.is_ok()));
        let on: Vec<_> = served.iter().map(|(_, _, on)| on.as_deref()).collect();
        assert_eq!(on, [Some("a-reader"); 3], "served on the thread that gave the bytes back");
        drop(waiting);
        assert_eq!(memory.used(), 0);
    }

    #[cfg(feature = "async")]
    #[test]
    fn a_waiter_on_no_thread_fails_limit_on_the_timer_once_its_patience_is_out() {
        let memory = Memory::new(100);
        let held = memory.try_reserve(100, "a read").unwrap();
        let (answers, answered) = std::sync::mpsc::channel();
        let _waiting = wait_for(&memory, "an upload", 50, Duration::from_millis(20), &answers);
        let (name, reserved, on) = answered.recv_timeout(Duration::from_secs(10)).unwrap();
        let error = reserved.unwrap_err();
        assert_eq!((name, error.kind(), error.fact("limit")), ("an upload", ErrorKind::Limit, Some(STORE_MEMORY_NOW)));
        assert_eq!(on.as_deref(), Some("tinystore-timer"));
        // the line is empty again: a reservation that fits takes its bytes at once
        drop(held);
        memory.try_reserve(100, "a read").unwrap();
    }

    #[cfg(feature = "async")]
    #[test]
    fn a_waiter_dropped_unanswered_leaves_the_line_and_the_one_behind_it_is_served() {
        let memory = Memory::new(100);
        let _held = memory.try_reserve(60, "a read").unwrap();
        let (answers, answered) = std::sync::mpsc::channel();
        let large = wait_for(&memory, "a merge", 80, Duration::from_secs(30), &answers);
        let small = wait_for(&memory, "a page", 30, Duration::from_secs(30), &answers);
        assert!(answered.try_recv().is_err(), "the page waits behind the merge");
        drop(large);
        let (name, reserved, _) = answered.try_recv().expect("the page was served as the merge left");
        assert_eq!((name, reserved.unwrap()), ("a page", 30));
        drop(small);
    }

    #[cfg(feature = "async")]
    #[test]
    fn a_waiter_on_no_thread_fails_closed_when_its_stores_timer_stops() {
        let timer = Arc::new(Timer::new());
        let memory = Memory::on(100, Arc::clone(&timer));
        let _held = memory.try_reserve(100, "a read").unwrap();
        let (answers, answered) = std::sync::mpsc::channel();
        let _waiting = wait_for(&memory, "an append", 10, Duration::from_secs(30), &answers);
        timer.stop();
        let (_, reserved, _) = answered.try_recv().expect("answered as the timer stopped");
        assert_eq!(reserved.unwrap_err().kind(), ErrorKind::Closed);
        let _late = wait_for(&memory, "an append", 10, Duration::from_secs(30), &answers);
        let (_, reserved, _) = answered.try_recv().expect("one that would wait after the stop is answered at once");
        assert_eq!(reserved.unwrap_err().kind(), ErrorKind::Closed);
    }

    #[cfg(feature = "async")]
    #[test]
    fn a_reservation_that_fits_is_answered_before_reserve_then_returns() {
        let memory = Memory::new(100);
        let (answers, answered) = std::sync::mpsc::channel();
        let _waiting = wait_for(&memory, "an append", 40, Duration::from_secs(30), &answers);
        let (_, reserved, on) = answered.try_recv().unwrap();
        assert_eq!(reserved.unwrap(), 40);
        assert_eq!(on, thread::current().name().map(str::to_owned));
        let too_large = wait_for(&memory, "an upload", 101, Duration::from_secs(30), &answers);
        assert_eq!(answered.try_recv().unwrap().1.unwrap_err().fact("limit"), Some(STORE_MEMORY));
        drop(too_large);
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
