//! What blobs' tests share: a store with no background work, on a clock the
//! test moves, and a look at what lies on disk and in blobs.db.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use super::engine::Blobs;
use super::files::Files;
use crate::{Durability, Options, Store, TestClock};

pub(crate) const HOUR: Duration = Duration::from_secs(60 * 60);

pub(crate) struct Fixture {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) store: Store,
    pub(crate) clock: Arc<TestClock>,
    durability: Option<Durability>,
}

/// A store without background work, on a clock the test moves.
pub(crate) fn fixture() -> Fixture {
    fixture_with(None)
}

/// The same, its files committed as `durability` says.
pub(crate) fn fixture_with(durability: Option<Durability>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)));
    let store = open(dir.path(), &clock, durability);
    Fixture { dir, store, clock, durability }
}

fn open(dir: &Path, clock: &Arc<TestClock>, durability: Option<Durability>) -> Store {
    let options = Options { clock: Some(clock.clone()), background: false, durability, ..Options::default() };
    Store::open(dir, options).unwrap()
}

impl Fixture {
    /// The store closed and opened again on the same directory and clock, as
    /// a restart does.
    pub(crate) fn reopen(self) -> Fixture {
        self.store.close().unwrap();
        let store = open(self.dir.path(), &self.clock, self.durability);
        Fixture { store, ..self }
    }

    pub(crate) fn files(&self, name: &str) -> Files {
        self.store.files(name).open().unwrap()
    }

    pub(crate) fn blobs(&self) -> Arc<Blobs> {
        Blobs::of(&self.store).unwrap()
    }

    /// The files of `objects/`, every directory of it.
    pub(crate) fn objects(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for fan in std::fs::read_dir(self.dir.path().join("blobs/objects")).unwrap() {
            for file in std::fs::read_dir(fan.unwrap().path()).unwrap() {
                found.push(file.unwrap().path());
            }
        }
        found
    }

    /// The files of `uploads/`.
    pub(crate) fn uploads(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.dir.path().join("blobs/uploads")).unwrap().map(|entry| entry.unwrap().path()).collect()
    }

    /// Rows of a table of blobs.db, counted on a reader.
    pub(crate) fn rows(&self, table: &str) -> i64 {
        let sql = format!("select count(*) from {table}");
        self.blobs().file.read(|connection| Ok(connection.query_row(&sql, [], |row| row.get(0)).unwrap())).unwrap()
    }
}

/// `size` bytes that differ from one place to the next, the same every run.
pub(crate) fn bytes(size: usize) -> Vec<u8> {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15 ^ size as u64;
    (0..size)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// Changes one byte of a file on disk, as a failing disk would.
pub(crate) fn flip(path: &Path, at: u64) {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new().read(true).write(true).open(path).unwrap();
    let mut byte = [0_u8];
    file.seek(SeekFrom::Start(at)).unwrap();
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Start(at)).unwrap();
    file.write_all(&[byte[0] ^ 0x5a]).unwrap();
}
