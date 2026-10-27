//! What metrics' tests share: a store with no background work, on a clock the
//! test moves, and a look at what lies in metrics.db.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::engine::Engine;
use super::{Metrics, Series};
use crate::{Options, Store, TestClock};

pub(crate) const SECOND: Duration = Duration::from_secs(1);
pub(crate) const MINUTE: Duration = Duration::from_mins(1);
pub(crate) const HOUR: Duration = Duration::from_hours(1);
pub(crate) const DAY: Duration = Duration::from_hours(24);

pub(crate) struct Fixture {
    pub(crate) store: Store,
    pub(crate) clock: Arc<TestClock>,
    // Last: fields drop in order, and Windows removes no file the store still holds.
    pub(crate) dir: tempfile::TempDir,
}

/// A store without background work, its clock at an hour's start.
pub(crate) fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)));
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

    pub(crate) fn metrics(&self) -> Metrics {
        self.store.metrics().unwrap()
    }

    pub(crate) fn now(&self) -> SystemTime {
        self.store.now()
    }

    /// Moves the clock ahead.
    pub(crate) fn pass(&self, span: Duration) {
        self.clock.set(self.now() + span).unwrap();
    }

    pub(crate) fn engine(&self) -> Arc<Engine> {
        Engine::of(&self.store).unwrap()
    }

    /// Rows of a table of metrics.db, counted on a reader.
    pub(crate) fn rows(&self, table: &str) -> i64 {
        let sql = format!("select count(*) from {table}");
        self.engine().file.read(|connection| Ok(connection.query_row(&sql, [], |row| row.get(0)).unwrap())).unwrap()
    }

    /// One value of metrics.db, read on a reader.
    pub(crate) fn value(&self, sql: &str) -> i64 {
        self.engine().file.read(|connection| Ok(connection.query_row(sql, [], |row| row.get(0)).unwrap())).unwrap()
    }

    /// A gauge of `count` samples a `step` apart, ending at the clock.
    pub(crate) fn gauge(&self, name: &str, count: usize, step: Duration, value: impl Fn(usize) -> f64) -> Series {
        let mut series = Series::gauge(name);
        let first = self.now() - step * (count as u32 - 1);
        series.extend((0..count).map(|index| (first + step * index as u32, value(index))));
        series
    }
}

/// Each series' samples as pairs of unix milliseconds and the bits of the
/// value, for comparing what was written with what was read.
pub(crate) fn bits(series: &Series) -> Vec<(i64, u64)> {
    series.samples().iter().map(|sample| (crate::unix_millis(sample.at), sample.value.to_bits())).collect()
}
