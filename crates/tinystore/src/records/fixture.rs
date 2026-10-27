//! What records' tests share: a store with no background work, on a clock
//! the test moves, and a look at what lies in records.db.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::engine::Records;
use super::event::Event;
use super::events::Events;
use crate::{Options, Store, TestClock};

pub(crate) const SECOND: Duration = Duration::from_secs(1);
pub(crate) const DAY: Duration = Duration::from_secs(24 * 60 * 60);

pub(crate) struct Fixture {
    pub(crate) store: Store,
    pub(crate) clock: Arc<TestClock>,
    // Last: fields drop in order, and Windows removes no file the store still holds.
    pub(crate) dir: tempfile::TempDir,
}

/// A store without background work, on a clock the test moves.
pub(crate) fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)));
    let store = open(dir.path(), &clock);
    Fixture { dir, store, clock }
}

/// A store on a copy of `records`, the bytes of a records.db, on a clock the
/// test moves from `at`.
pub(crate) fn fixture_of(records: &[u8], at: SystemTime) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("records.db"), records).unwrap();
    let clock = Arc::new(TestClock::new(at));
    let store = open(dir.path(), &clock);
    Fixture { dir, store, clock }
}

fn open(dir: &Path, clock: &Arc<TestClock>) -> Store {
    Store::open(dir, Options { clock: Some(clock.clone()), background: false, ..Options::default() }).unwrap()
}

impl Fixture {
    /// The store closed and opened again on the same directory and clock, as
    /// a restart does.
    pub(crate) fn reopen(self) -> Fixture {
        self.store.close().unwrap();
        let store = open(self.dir.path(), &self.clock);
        Fixture { store, ..self }
    }

    pub(crate) fn events(&self, stream: &str) -> Events {
        self.store.events(stream).open().unwrap()
    }

    pub(crate) fn now(&self) -> SystemTime {
        self.store.now()
    }

    /// Moves the clock ahead.
    pub(crate) fn pass(&self, span: Duration) {
        self.clock.set(self.now() + span).unwrap();
    }

    pub(crate) fn records(&self) -> Arc<Records> {
        Records::of(&self.store).unwrap()
    }

    /// Holds the codec every append compresses with on a thread of its own,
    /// as an append compressing its rows does, until the sender it returns
    /// is dropped, or five seconds passed.
    #[cfg(feature = "async")]
    pub(crate) fn hold_codec(&self) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let (records, (release, released), (holding, held)) =
            (self.records(), std::sync::mpsc::channel::<()>(), std::sync::mpsc::channel());
        let holder = std::thread::spawn(move || {
            let _codec = records.codec.lock().unwrap();
            holding.send(()).unwrap();
            let _ = released.recv_timeout(Duration::from_secs(5));
        });
        held.recv().unwrap();
        (release, holder)
    }

    /// Rows of a table of records.db, counted on a reader.
    pub(crate) fn rows(&self, table: &str) -> i64 {
        let sql = format!("select count(*) from {table}");
        self.records().file.read(|connection| Ok(connection.query_row(&sql, [], |row| row.get(0)).unwrap())).unwrap()
    }
}

/// The names of events, in their order.
pub(crate) fn names(events: &[Event]) -> Vec<&str> {
    events.iter().map(|event| event.name.as_str()).collect()
}
