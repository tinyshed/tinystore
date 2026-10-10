//! Content ids, handed out from a block reserved in `meta`, so that an id is
//! never given twice, not after a crash either. An id is held until its
//! upload ends, so that the settled mark stays below every id whose file may
//! still lack its row.

use std::collections::HashSet;
use std::sync::Mutex;

use super::engine::lock;
use crate::Result;

/// Content ids reserved in one write of `meta`.
const BLOCK: i64 = 1000;

#[derive(Default)]
pub(crate) struct Ids {
    state: Mutex<State>,
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
}

impl Ids {
    /// Starts from what the file reserved: every id up to it may be taken.
    pub(crate) fn start(&self, reserved: i64) {
        let mut state = lock(&self.state);
        state.last = reserved;
        state.end = reserved;
    }

    /// The next id, held; `reserve` writes the next block when this one is
    /// spent and returns its last id.
    pub(crate) fn take(&self, reserve: impl FnOnce(i64) -> Result<i64>) -> Result<i64> {
        let mut state = lock(&self.state);
        if state.last == state.end {
            let end = reserve(BLOCK)?;
            state.last = end - BLOCK;
            state.end = end;
        }
        state.last += 1;
        let id = state.last;
        state.held.insert(id);
        Ok(id)
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
}
