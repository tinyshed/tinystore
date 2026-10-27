//! What kv's tests share: a store with no background work, on a clock the test
//! moves, and a look at kv.db's rows.

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::engine::Kv;
use crate::{Options, Store, TestClock};

pub(crate) const SECOND: Duration = Duration::from_secs(1);
pub(crate) const MINUTE: Duration = Duration::from_secs(60);
pub(crate) const HOUR: Duration = Duration::from_secs(60 * 60);
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
    let store = open(&dir, &clock);
    Fixture { dir, store, clock }
}

fn open(dir: &tempfile::TempDir, clock: &Arc<TestClock>) -> Store {
    let options = Options { clock: Some(clock.clone()), background: false, ..Options::default() };
    Store::open(dir.path(), options).unwrap()
}

impl Fixture {
    /// The store closed and opened again on the same directory and clock, as a
    /// restart does.
    pub(crate) fn reopen(self) -> Fixture {
        self.store.close().unwrap();
        let store = open(&self.dir, &self.clock);
        Fixture { store, ..self }
    }

    /// Rows of a kv table, counted on a reader.
    pub(crate) fn rows(&self, table: &str) -> i64 {
        let kv = Kv::of(&self.store).unwrap();
        let sql = format!("select count(*) from {table}");
        kv.file().read(|connection| Ok(connection.query_row(&sql, [], |row| row.get(0)).unwrap())).unwrap()
    }

    /// The commits kv.db made so far.
    pub(crate) fn commits(&self) -> u64 {
        Kv::of(&self.store).unwrap().file().commits()
    }

    /// kv.db's writer, held on a thread of its own until what this returns is
    /// dropped, ten seconds at most: a call that waited for it on the test's
    /// thread hangs that long, not for ever.
    #[cfg(feature = "async")]
    pub(crate) fn hold_writer(&self) -> WriterHeld {
        let kv = Kv::of(&self.store).unwrap();
        let (release, held_until) = std::sync::mpsc::channel::<()>();
        let (holding, held) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _writer = kv.file().hold_writer();
            holding.send(()).unwrap();
            let _ = held_until.recv_timeout(Duration::from_secs(10));
        });
        held.recv().unwrap();
        WriterHeld { release: Some(release), holder: Some(holder) }
    }
}

/// kv.db's writer held, let go as this is dropped.
#[cfg(feature = "async")]
pub(crate) struct WriterHeld {
    release: Option<std::sync::mpsc::Sender<()>>,
    holder: Option<std::thread::JoinHandle<()>>,
}

#[cfg(feature = "async")]
impl Drop for WriterHeld {
    fn drop(&mut self) {
        drop(self.release.take());
        if let Some(holder) = self.holder.take() {
            holder.join().unwrap();
        }
    }
}

/// Waits until `holds`, failing the test after ten seconds, as long as a
/// thread of the store's may take on a busy machine.
#[cfg(feature = "async")]
pub(crate) fn eventually(what: &str, mut holds: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !holds() {
        assert!(std::time::Instant::now() < deadline, "{what}: not within ten seconds");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Session {
    pub(crate) user: i64,
    pub(crate) device: String,
}

pub(crate) fn session(user: i64) -> Session {
    Session { user, device: "phone".to_owned() }
}
