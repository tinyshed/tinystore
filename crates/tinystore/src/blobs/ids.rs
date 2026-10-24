//! Content ids, handed out from a block reserved in `meta`, so that an id is
//! never given twice, not after a crash either. An id is held until its
//! upload ends, so that the settled mark stays below every id whose file may
//! still lack its row.

use std::collections::HashSet;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

use crate::sync::lock;
use crate::{Error, Result};

/// Content ids reserved in one write of `meta`.
const BLOCK: i64 = 1000;

#[derive(Default)]
pub(crate) struct Ids {
    state: Mutex<State>,
    /// Woken as a block's write ends, for the takers that waited for it.
    reserved: Condvar,
}

#[derive(Default)]
struct State {
    /// The last id handed out.
    last: i64,
    /// The last id of the block reserved.
    end: i64,
    held: HashSet<i64>,
    /// Ids whose commit failed with its outcome unknown, held until the file
    /// says whether their content is there.
    doubtful: HashSet<i64>,
    /// Whether a taker writes the next block now, the lock let go meanwhile.
    reserving: bool,
}

impl Ids {
    /// Starts from what the file reserved: every id up to it may have been
    /// taken.
    pub(crate) fn start(&self, reserved: i64) {
        let mut state = lock(&self.state);
        state.last = reserved;
        state.end = reserved;
    }

    /// The next id, held; `reserve` writes the next block when this one is
    /// spent and returns its last id.
    ///
    /// The block's write is a commit, and the lock is let go while it runs:
    /// every `let_go` would wait for that commit otherwise, and so would an
    /// awaited write's [`Ids::try_take`] in its poll. One taker writes a
    /// block at a time, and the others wait for it.
    pub(crate) fn take(&self, reserve: impl FnOnce(i64) -> Result<i64>) -> Result<i64> {
        let mut reserve = Some(reserve);
        let mut state = lock(&self.state);
        while state.last == state.end {
            if state.reserving {
                state = self.reserved.wait(state).unwrap_or_else(PoisonError::into_inner);
                continue;
            }
            // a block this taker wrote is its own until it takes an id of it
            let Some(reserve) = reserve.take() else {
                return Err(Error::internal("blobs: a taker of a content id wrote two blocks"));
            };
            let writing = Reserving::begin(self, state);
            let reserved = reserve(BLOCK);
            state = writing.end();
            let end = reserved?;
            state.last = end - BLOCK;
            state.end = end;
        }
        Ok(state.hold_next())
    }

    /// The next id, held, when the block reserved has one left: what a poll
    /// takes, since writing the next block is a commit.
    #[cfg(feature = "async")]
    pub(crate) fn try_take(&self) -> Option<i64> {
        let mut state = lock(&self.state);
        (state.last < state.end).then(|| state.hold_next())
    }

    /// Ends an id's hold: its content committed, or its file is gone.
    pub(crate) fn let_go(&self, id: i64) {
        let mut state = lock(&self.state);
        state.held.remove(&id);
        state.doubtful.remove(&id);
    }

    /// Keeps an id held until maintenance learns what became of it.
    pub(crate) fn doubt(&self, id: i64) {
        let mut state = lock(&self.state);
        state.held.insert(id);
        state.doubtful.insert(id);
    }

    pub(crate) fn holds(&self, id: i64) -> bool {
        lock(&self.state).held.contains(&id)
    }

    pub(crate) fn doubtful(&self) -> Vec<i64> {
        lock(&self.state).doubtful.iter().copied().collect()
    }

    /// The highest id at or below which every id is committed or has no file:
    /// one below the first id held, else the last handed out.
    pub(crate) fn settled_mark(&self) -> i64 {
        let state = lock(&self.state);
        state.held.iter().map(|id| id - 1).fold(state.last, i64::min)
    }
}

impl State {
    fn hold_next(&mut self) -> i64 {
        self.last += 1;
        self.held.insert(self.last);
        self.last
    }
}

/// A taker's write of the next block, the lock let go while it runs. It
/// ends however the write does, a panic of `reserve` included: the takers
/// waiting for it would wait for ever otherwise, since none of them would
/// write the block it left unwritten.
struct Reserving<'i> {
    ids: &'i Ids,
    ended: bool,
}

impl<'i> Reserving<'i> {
    fn begin(ids: &'i Ids, mut state: MutexGuard<'i, State>) -> Reserving<'i> {
        state.reserving = true;
        Reserving { ids, ended: false }
    }

    /// The state again, the write ended and its waiters woken under its lock,
    /// so that none of them begins another block before this one is in.
    fn end(mut self) -> MutexGuard<'i, State> {
        self.ended = true;
        self.ids.reserving_ended()
    }
}

impl Drop for Reserving<'_> {
    fn drop(&mut self) {
        if !self.ended {
            drop(self.ids.reserving_ended());
        }
    }
}

impl Ids {
    fn reserving_ended(&self) -> MutexGuard<'_, State> {
        let mut state = lock(&self.state);
        state.reserving = false;
        self.reserved.notify_all();
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_is_never_given_twice_and_the_mark_stays_below_what_is_held() {
        let ids = Ids::default();
        ids.start(2000);
        let reserved = std::cell::Cell::new(0);
        let reserve = |block| {
            reserved.set(reserved.get() + 1);
            Ok(2000 + block)
        };
        let first = ids.take(reserve).unwrap();
        let second = ids.take(|_| unreachable!("the block has room")).unwrap();
        assert_eq!((first, second, reserved.get()), (2001, 2002, 1));
        assert_eq!(ids.settled_mark(), 2000);
        ids.let_go(first);
        assert_eq!(ids.settled_mark(), 2001);
        ids.let_go(second);
        assert_eq!(ids.settled_mark(), 2002);
        ids.doubt(second);
        assert_eq!(ids.settled_mark(), 2001);
        assert_eq!(ids.doubtful(), [second]);
    }

    #[test]
    fn takers_that_find_the_block_spent_wait_for_the_one_that_writes_the_next_and_none_waits_on_the_lock() {
        use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier, mpsc};
        use std::time::Duration;

        let ids = Arc::new(Ids::default());
        ids.start(0);
        let (reserved, writes) = (Arc::new(AtomicI64::new(0)), Arc::new(AtomicUsize::new(0)));
        let (writing, together) = (Arc::new(AtomicBool::new(false)), Arc::new(Barrier::new(8)));
        let (go_on, going_on) = mpsc::channel::<()>();
        let going_on = Arc::new(Mutex::new(going_on));
        let takers: Vec<_> = (0..8)
            .map(|_| {
                let (ids, reserved, writes) = (Arc::clone(&ids), Arc::clone(&reserved), Arc::clone(&writes));
                let (writing, together, going_on) =
                    (Arc::clone(&writing), Arc::clone(&together), Arc::clone(&going_on));
                std::thread::spawn(move || {
                    together.wait();
                    ids.take(|block| {
                        writes.fetch_add(1, Ordering::SeqCst);
                        writing.store(true, Ordering::SeqCst);
                        // a commit's while, until the test says it is over, or five seconds
                        let _ = lock(&going_on).recv_timeout(Duration::from_secs(5));
                        writing.store(false, Ordering::SeqCst);
                        Ok(reserved.fetch_add(block, Ordering::SeqCst) + block)
                    })
                    .unwrap()
                })
            })
            .collect();
        while !writing.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        ids.let_go(999_999);
        assert!(writing.load(Ordering::SeqCst), "a let go waited for the block's write");
        go_on.send(()).unwrap();
        let mut taken: Vec<i64> = takers.into_iter().map(|taker| taker.join().unwrap()).collect();
        taken.sort_unstable();
        assert_eq!(taken, (1..=8).collect::<Vec<_>>(), "each id once");
        assert_eq!(writes.load(Ordering::SeqCst), 1, "one block written for the eight");
    }

    #[test]
    fn a_block_write_that_panics_leaves_the_next_taker_free_to_write_one() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        use std::sync::{Arc, mpsc};
        use std::time::Duration;

        let ids = Arc::new(Ids::default());
        ids.start(0);
        // as when the taker leads blobs.db's group and another write's answer panics
        let panicked = catch_unwind(AssertUnwindSafe(|| ids.take(|_| panic!("a write of the group panicked"))));
        assert!(panicked.is_err());
        let (taken, took) = mpsc::channel();
        let taker = Arc::clone(&ids);
        // on a thread of its own, so that a taker left waiting fails the test rather than hangs it
        std::thread::spawn(move || taken.send(taker.take(Ok)));
        let id = took.recv_timeout(Duration::from_secs(10)).expect("the next taker waits for a write that ended");
        assert_eq!(id.unwrap(), 1);
    }

    #[cfg(feature = "async")]
    #[test]
    fn a_poll_takes_an_id_only_from_a_block_written_already() {
        let ids = Ids::default();
        ids.start(BLOCK);
        assert_eq!(ids.try_take(), None, "an open's block is spent: the next is a commit");
        ids.take(|block| Ok(BLOCK + block)).unwrap();
        assert_eq!(ids.try_take(), Some(BLOCK + 2));
    }
}
