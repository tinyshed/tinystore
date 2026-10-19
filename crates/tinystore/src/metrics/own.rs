//! The store's own metrics, plan/self-metrics.md: the listener of the
//! store's probe, which counts what the probe tells into instruments of the
//! store's, and reads the gauges of its files, its memory and its disk as
//! the instruments' totals are written.
//!
//! The totals' write is unmeasured, so that it measures nothing of itself;
//! what a program writes into `metrics.db` is measured as any other write.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::Metrics;
use super::instruments::{Counter, Gauge, Timer};
use crate::probe::Listen;
use crate::store::WeakStore;
use crate::{Result, Store};

/// The prefix of the names the store writes of itself.
pub(crate) const PREFIX: &str = "tinystore_";
/// The application's databases a store names in its `file` label; every one
/// after them counts into `sql/__overflow`.
const NAMED_DATABASES: usize = 32;
const OVERFLOW: &str = "sql/__overflow";

/// Opens the store's metrics engine and makes it the probe's listener.
pub(crate) fn measure_the_store(store: &Store) -> Result<()> {
    let metrics = store.metrics()?;
    let own = Arc::new(Own::open(&metrics, store)?);
    let gauges = Arc::downgrade(&own);
    metrics.engine.before_each_write(Box::new(move || {
        if let Some(own) = gauges.upgrade() {
            own.read_gauges();
        }
    }));
    store.probe().listen(own);
    Ok(())
}

struct Own {
    store: WeakStore,
    commits: Counter,
    commit_writes: Counter,
    commit_ms: Timer,
    writer_wait_ms: Timer,
    background_failures: Counter,
    file_bytes: Gauge,
    wal_bytes: Gauge,
    readers_open: Gauge,
    memory_used: Gauge,
    memory_limit: Gauge,
    disk_free: Gauge,
    /// Each file's instruments of its commits, by the label it is named by.
    files: Mutex<Files>,
}

#[derive(Default)]
struct Files {
    by_label: HashMap<String, Commits>,
    /// The databases named by their own label.
    named: HashSet<String>,
}

#[derive(Clone)]
struct Commits {
    commits: Counter,
    writes: Counter,
    commit_ms: Timer,
    wait_ms: Timer,
}

impl Own {
    fn open(metrics: &Metrics, store: &Store) -> Result<Self> {
        let counter = |name: &str, help: &str| metrics.counter(name).own().help(help).open();
        let gauge = |name: &str, unit: &str, help: &str| metrics.gauge(name).own().unit(unit).help(help).open();
        let timer = |name: &str, help: &str| metrics.timer(name).own().help(help).open();
        Ok(Self {
            store: store.downgrade(),
            commits: counter("tinystore_commits_total", "Commits of a file's writer.")?,
            commit_writes: counter("tinystore_commit_writes_total", "The writes those commits carried.")?,
            commit_ms: timer("tinystore_commit_ms", "A commit of the local file, its sync included.")?,
            writer_wait_ms: timer("tinystore_writer_wait_ms", "A write's wait for the commit that carries it.")?,
            background_failures: counter("tinystore_background_failures_total", "Failures of periodic work.")?,
            file_bytes: gauge("tinystore_file_bytes", "bytes", "A file's size.")?,
            wal_bytes: gauge("tinystore_wal_bytes", "bytes", "A file's write-ahead log's size.")?,
            readers_open: gauge("tinystore_readers_open", "connections", "A file's read connections open.")?,
            memory_used: gauge("tinystore_memory_used_bytes", "bytes", "What the store's work holds.")?,
            memory_limit: gauge("tinystore_memory_limit_bytes", "bytes", "The store's memory budget.")?,
            disk_free: gauge("tinystore_disk_free_bytes", "bytes", "Free space under the store's directory.")?,
            files: Mutex::default(),
        })
    }

    /// The instruments of a file's commits, by the label it is named by.
    fn of_file(&self, file: &str) -> Commits {
        let mut files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(commits) = files.by_label.get(file) {
            return commits.clone();
        }
        let label = files.label(file);
        let labels = [("file", label.as_str())];
        let commits = Commits {
            commits: self.commits.with(&labels),
            writes: self.commit_writes.with(&labels),
            commit_ms: self.commit_ms.with(&labels),
            wait_ms: self.writer_wait_ms.with(&labels),
        };
        files.by_label.insert(file.to_owned(), commits.clone());
        commits
    }

    /// Sets the gauges as the totals are about to be written.
    fn read_gauges(&self) {
        let Some(store) = self.store.upgrade() else { return };
        // the databases past those named add up under one label
        let mut by_label: HashMap<String, [f64; 3]> = HashMap::new();
        for file in store.probe().files() {
            let label = self.files.lock().unwrap_or_else(PoisonError::into_inner).label(&file.file);
            let sums = by_label.entry(label).or_default();
            sums[0] += size(&file.path);
            sums[1] += size(&wal_of(&file.path));
            sums[2] += file.readers as f64;
        }
        for (label, [bytes, wal, readers]) in by_label {
            let labels = [("file", label.as_str())];
            self.file_bytes.with(&labels).set(bytes);
            self.wal_bytes.with(&labels).set(wal);
            self.readers_open.with(&labels).set(readers);
        }
        let memory = store.memory();
        self.memory_used.set(memory.used() as f64);
        if memory.bounded() {
            self.memory_limit.set(memory.limit() as f64);
        }
        if let Ok(free) = crate::space::available(store.dir()) {
            self.disk_free.set(free as f64);
        }
    }
}

impl Files {
    /// A file's label: its own, or `sql/__overflow` for a database past the
    /// ones the store names.
    fn label(&mut self, file: &str) -> String {
        if !file.starts_with("sql/") || self.named.contains(file) {
            return file.to_owned();
        }
        if self.named.len() >= NAMED_DATABASES {
            return OVERFLOW.to_owned();
        }
        self.named.insert(file.to_owned());
        file.to_owned()
    }
}

impl Listen for Own {
    fn committed(&self, file: &str, writes: usize, took: Duration, waits: &[Duration]) {
        let commits = self.of_file(file);
        commits.commits.inc();
        commits.writes.add(writes as f64);
        commits.commit_ms.record(took);
        for wait in waits {
            commits.wait_ms.record(*wait);
        }
    }

    fn failed(&self, task: &str) {
        self.background_failures.with(&[("task", task)]).inc();
    }
}

/// A file's size in bytes, 0 for one not there.
fn size(path: &std::path::Path) -> f64 {
    std::fs::metadata(path).map_or(0.0, |meta| meta.len() as f64)
}

fn wal_of(path: &std::path::Path) -> std::path::PathBuf {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    wal.into()
}

#[cfg(test)]
#[path = "own_tests.rs"]
mod own_tests;

#[cfg(test)]
mod tests {
    use super::{Files, NAMED_DATABASES, OVERFLOW};

    #[test]
    fn a_database_past_those_named_counts_into_the_overflow() {
        let mut files = Files::default();
        assert_eq!(files.label("kv"), "kv");
        for n in 0..NAMED_DATABASES {
            assert_eq!(files.label(&format!("sql/db{n}")), format!("sql/db{n}"));
        }
        assert_eq!(files.label("sql/one-too-many"), OVERFLOW);
        assert_eq!(files.label("sql/db0"), "sql/db0");
        assert_eq!(files.label("jobs"), "jobs");
    }
}
