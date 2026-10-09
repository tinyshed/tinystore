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
    pub(crate) dir: tempfile::TempDir,
    pub(crate) store: Store,
    pub(crate) clock: Arc<TestClock>,
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Session {
    pub(crate) user: i64,
    pub(crate) device: String,
}

pub(crate) fn session(user: i64) -> Session {
    Session { user, device: "phone".to_owned() }
}
