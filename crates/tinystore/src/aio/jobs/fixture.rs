//! What the tests of jobs' async calls share: an awaited store with no
//! background work, on a clock the test moves, and a stream's next item.

use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use futures_core::Stream;
use serde::{Deserialize, Serialize};

use crate::aio::Store;
use crate::{Options, TestClock};

pub(super) const SECOND: Duration = Duration::from_secs(1);
pub(super) const MINUTE: Duration = Duration::from_secs(60);
pub(super) const HOUR: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Draft {
    pub(super) chat: i64,
    pub(super) text: String,
}

pub(super) fn draft(chat: i64) -> Draft {
    Draft { chat, text: format!("a draft for chat {chat}") }
}

pub(super) struct Fixture {
    pub(super) _dir: tempfile::TempDir,
    pub(super) store: Store,
    pub(super) clock: Arc<TestClock>,
}

/// An awaited store without background work, on a clock the test moves, at
/// 2026-10-09 12:00 UTC.
pub(super) fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_791_547_200)));
    let options = Options { clock: Some(clock.clone()), background: false, ..Options::default() };
    let store = Store::open(dir.path(), options).unwrap();
    Fixture { _dir: dir, store, clock }
}

impl Fixture {
    /// Moves the clock on, and tells the store, so that a worker asleep until
    /// a time reads it again.
    pub(super) fn advance(&self, by: Duration) {
        self.clock.advance(by);
        self.store.blocking().clock_moved();
    }
}

/// A stream's next item, as `StreamExt::next` gives it.
pub(super) async fn next<S: Stream + Unpin>(stream: &mut S) -> Option<S::Item> {
    std::future::poll_fn(|context| Pin::new(&mut *stream).poll_next(context)).await
}

/// Waits until `holds` does, the executor free meanwhile, failing the test
/// after ten seconds.
pub(super) async fn eventually(what: &str, mut holds: impl FnMut() -> bool) {
    let deadline = Instant::now() + crate::testing::PATIENCE;
    while !holds() {
        assert!(Instant::now() < deadline, "{what}: not within {:?}", crate::testing::PATIENCE);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}
