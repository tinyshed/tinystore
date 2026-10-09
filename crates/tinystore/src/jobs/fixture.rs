//! What jobs' tests share: a store with no background work, on a clock the
//! test moves.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{Options, Store, TestClock};

pub(crate) const SECOND: Duration = Duration::from_secs(1);
pub(crate) const MINUTE: Duration = Duration::from_secs(60);
pub(crate) const HOUR: Duration = Duration::from_secs(60 * 60);
pub(crate) const DAY: Duration = Duration::from_secs(24 * 60 * 60);

pub(crate) struct Fixture {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) store: Store,
    pub(crate) clock: Arc<TestClock>,
}

/// A store without background work, on a clock the test moves, at 2026-10-09
/// 12:00 UTC.
pub(crate) fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_791_547_200)));
    let store = open(&dir, &clock);
    Fixture { dir, store, clock }
}

impl Fixture {
    pub(crate) fn now(&self) -> SystemTime {
        self.store.now()
    }

    /// Moves the clock on, and tells the store, as a test's `server.clock` does.
    pub(crate) fn advance(&self, by: Duration) {
        self.clock.advance(by);
        self.store.clock_moved();
    }

    /// Closes the store and opens the directory again, on the same clock.
    pub(crate) fn reopen(&mut self) {
        self.store.close().unwrap();
        self.store = open(&self.dir, &self.clock);
    }
}

fn open(dir: &tempfile::TempDir, clock: &Arc<TestClock>) -> Store {
    let options = Options { clock: Some(clock.clone()), background: false, ..Options::default() };
    Store::open(dir.path(), options).unwrap()
}
