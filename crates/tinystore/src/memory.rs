use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::{Error, Result};

/// The bytes a store's work may hold at once.
///
/// Work reserves before it allocates and gives the bytes back by dropping its
/// reservation. A request past the limit is a `Limit` error naming what asked:
/// the caller gets a refusal, never a quietly smaller answer. The budget counts
/// what work reserves; it is not a ceiling on the process's RSS.
pub struct Memory {
    limit: u64,
    used: Mutex<u64>,
    freed: Condvar,
}

impl Memory {
    pub fn new(limit: u64) -> Arc<Self> {
        Arc::new(Self { limit, used: Mutex::new(0), freed: Condvar::new() })
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    pub fn used(&self) -> u64 {
        *self.lock()
    }

    /// Takes `bytes` now, or fails with `Limit`.
    pub fn try_reserve(self: &Arc<Self>, bytes: u64, what: &str) -> Result<Reservation> {
        self.refuse_past_limit(bytes, what)?;
        let mut used = self.lock();
        if *used + bytes > self.limit {
            return Err(self.exhausted(bytes, *used, what));
        }
        *used += bytes;
        Ok(self.reservation(bytes))
    }

    /// Takes `bytes`, waiting up to `patience` for other work to give them back.
    pub fn reserve(self: &Arc<Self>, bytes: u64, what: &str, patience: Duration) -> Result<Reservation> {
        self.refuse_past_limit(bytes, what)?;
        let deadline = Instant::now() + patience;
        let mut used = self.lock();
        while *used + bytes > self.limit {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(self.exhausted(bytes, *used, what));
            }
            used = self.freed.wait_timeout(used, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        *used += bytes;
        Ok(self.reservation(bytes))
    }

    fn refuse_past_limit(&self, bytes: u64, what: &str) -> Result<()> {
        if bytes <= self.limit {
            return Ok(());
        }
        Err(Error::limit(format!("{what}: {bytes} bytes is more than the store's memory, {} bytes", self.limit)))
    }

    fn exhausted(&self, bytes: u64, used: u64, what: &str) -> Error {
        Error::limit(format!(
            "{what}: {bytes} bytes do not fit in the store's memory, {used} of {} bytes in use",
            self.limit
        ))
    }

    fn reservation(self: &Arc<Self>, bytes: u64) -> Reservation {
        Reservation { memory: Arc::clone(self), bytes }
    }

    fn give_back(&self, bytes: u64) {
        let mut used = self.lock();
        *used -= bytes;
        drop(used);
        self.freed.notify_all();
    }

    fn lock(&self) -> MutexGuard<'_, u64> {
        self.used.lock().unwrap_or_else(PoisonError::into_inner)
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

    #[test]
    fn a_request_larger_than_the_whole_budget_fails_without_waiting() {
        let memory = Memory::new(100);
        let started = Instant::now();
        let error = memory.reserve(101, "upload", Duration::from_secs(5)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

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

    #[test]
    fn shrinking_gives_back_the_difference() {
        let memory = Memory::new(100);
        let mut reservation = memory.try_reserve(90, "decode").unwrap();
        reservation.shrink(40);
        assert_eq!(memory.used(), 40);
        drop(reservation);
        assert_eq!(memory.used(), 0);
    }
}
