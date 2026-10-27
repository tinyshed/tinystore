//! What search's tests share: a store without background work, and numbers
//! that look random and come again from their seed.

use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::index::Index;
use crate::{Durability, Options, Store};

pub(crate) struct Fixture {
    pub(crate) store: Store,
    // Last: fields drop in order, and Windows removes no file the store still holds.
    pub(crate) dir: tempfile::TempDir,
}

/// A store without background work, so that a test packs and purges itself.
pub(crate) fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    Fixture { dir, store }
}

pub(crate) fn open(dir: &Path) -> Store {
    // Commits to the operating system alone: a test's thousands of sets wait for no disk.
    let options = Options { background: false, durability: Some(Durability::Os), ..Options::default() };
    Store::open(dir, options).unwrap()
}

impl Fixture {
    /// The index `name` of fields `title`, weighing 2, and `body`.
    pub(crate) fn articles<T: Serialize + DeserializeOwned>(&self, name: &str) -> Index<T> {
        self.store.index(name).field("title", 2.0).field("body", 1.0).open().unwrap()
    }
}

/// SplitMix64: numbers that look random, the same again from the same seed.
pub(crate) struct Random(u64);

impl Random {
    pub(crate) fn new(seed: u64) -> Random {
        Random(seed)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number in `[0, n)`.
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    /// A number in `[0, n)` that comes small far more often than large, as
    /// words of a language do.
    pub(crate) fn skewed(&mut self, n: u64) -> u64 {
        let a = self.below(n);
        self.below(a + 1)
    }
}
