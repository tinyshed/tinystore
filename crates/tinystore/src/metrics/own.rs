//! The store's own metrics, docs/guides/metrics/self-metrics.md: the listener of the
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
use crate::probe::{Labels, Listen};
use crate::store::WeakStore;
use crate::{Result, Store};

/// The prefix of the names the store writes of itself.
pub(crate) const PREFIX: &str = "tinystore_";
/// The application's databases a store names in its `file` label; every one
/// after them counts into `sql/__overflow`.
const NAMED_DATABASES: usize = 32;
const OVERFLOW: &str = "sql/__overflow";

/// What a store measures of itself, when it is opened with its own metrics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelfMetrics {
    /// The machine it runs on as well: its process's memory and processor,
    /// the system's memory, swap, processors by core and load. A library
    /// built without the feature `system` measures none of it.
    pub system: bool,
}

impl Default for SelfMetrics {
    fn default() -> Self {
        Self { system: true }
    }
}

/// Opens the store's metrics engine and makes it the probe's listener.
///
/// Every instrument opens here, as the store's own work: a synchronous home
/// makes their names without waiting for a copy, and no measurement opens one
/// later, inside another file's commit.
pub(crate) fn measure_the_store(store: &Store, asked: SelfMetrics) -> Result<()> {
    let metrics = store.metrics()?;
    let own = Arc::new(crate::sqlite::own_work(|| Own::open(&metrics, store, asked))?);
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
    #[cfg(feature = "system")]
    machine: Option<super::system::Machine>,
    /// The engines' instruments, each set of labels as it is first told of.
    named: Mutex<Named>,
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
    #[cfg_attr(not(feature = "system"), expect(unused_variables, reason = "the machine is the feature system's"))]
    fn open(metrics: &Metrics, store: &Store, asked: SelfMetrics) -> Result<Self> {
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
            #[cfg(feature = "system")]
            machine: asked.system.then(|| super::system::Machine::open(metrics)).transpose()?,
            named: Mutex::new(Named::open(metrics)?),
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
        #[cfg(feature = "system")]
        if let Some(machine) = &self.machine {
            machine.read();
        }
        store.probe().sample();
    }

    /// The instrument of `name` and `labels`; none for a name the table does
    /// not hold, said once.
    fn named(&self, name: &'static str, labels: Labels<'_>) -> Option<Opened> {
        let mut named = self.named.lock().unwrap_or_else(PoisonError::into_inner);
        let mut key = name.to_owned();
        for (label, value) in labels {
            key.extend([char::from(31)].into_iter().chain(label.chars()).chain(['=']).chain(value.chars()));
        }
        if let Some(opened) = named.by_set.get(&key) {
            return Some(opened.clone());
        }
        let Some(base) = named.by_name.get(name).cloned() else {
            if named.unknown.insert(name) {
                tracing::warn!(target: "tinystore", name, "a store's own metric its table does not name");
            }
            return None;
        };
        let bounded: Vec<(&str, String)> =
            labels.iter().map(|(label, value)| (*label, named.bound(name, label, value))).collect();
        let pairs: Vec<(&str, &str)> = bounded.iter().map(|(label, value)| (*label, value.as_str())).collect();
        let opened = base.with(&pairs);
        named.by_set.insert(key, opened.clone());
        Some(opened)
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

    fn count(&self, name: &'static str, labels: Labels<'_>, by: f64) {
        if let Some(Opened::Counter(counter)) = self.named(name, labels) {
            counter.add(by);
        }
    }

    fn time(&self, name: &'static str, labels: Labels<'_>, took: Duration) {
        if let Some(Opened::Timer(timer)) = self.named(name, labels) {
            timer.record(took);
        }
    }

    fn set(&self, name: &'static str, labels: Labels<'_>, value: f64) {
        if let Some(Opened::Gauge(gauge)) = self.named(name, labels) {
            gauge.set(value);
        }
    }

    fn shift(&self, name: &'static str, labels: Labels<'_>, delta: f64) {
        if let Some(Opened::Gauge(gauge)) = self.named(name, labels) {
            gauge.add(delta);
        }
    }
}

/// The values a label of an engine's own metric names before the rest count
/// into `__overflow`: a store of many queues or indexes stays a few series.
const NAMED_VALUES: usize = 32;

/// The engines' instruments the store opened, and the values each of their
/// labels named.
struct Named {
    by_name: HashMap<&'static str, Opened>,
    by_set: HashMap<String, Opened>,
    values: HashMap<(&'static str, &'static str), HashSet<String>>,
    unknown: HashSet<&'static str>,
}

impl Named {
    /// Every instrument the table names.
    fn open(metrics: &Metrics) -> Result<Named> {
        let mut by_name = HashMap::new();
        for &(name, kind, unit, help) in NAMES {
            let opened = match kind {
                Made::Counter => Opened::Counter(metrics.counter(name).own().unit(unit).help(help).open()?),
                Made::Gauge => Opened::Gauge(metrics.gauge(name).own().unit(unit).help(help).open()?),
                Made::Timer => Opened::Timer(metrics.timer(name).own().help(help).open()?),
            };
            by_name.insert(name, opened);
        }
        Ok(Named { by_name, by_set: HashMap::new(), values: HashMap::new(), unknown: HashSet::new() })
    }

    fn bound(&mut self, name: &'static str, label: &'static str, value: &str) -> String {
        let named = self.values.entry((name, label)).or_default();
        if named.contains(value) {
            return value.to_owned();
        }
        if named.len() >= NAMED_VALUES {
            return "__overflow".to_owned();
        }
        named.insert(value.to_owned());
        value.to_owned()
    }
}

#[derive(Clone)]
enum Opened {
    Counter(Counter),
    Gauge(Gauge),
    Timer(Timer),
}

impl Opened {
    fn with(&self, labels: &[(&str, &str)]) -> Opened {
        match self {
            Opened::Counter(counter) => Opened::Counter(counter.with(labels)),
            Opened::Gauge(gauge) => Opened::Gauge(gauge.with(labels)),
            Opened::Timer(timer) => Opened::Timer(timer.with(labels)),
        }
    }
}

#[derive(Clone, Copy)]
enum Made {
    Counter,
    Gauge,
    Timer,
}

/// Every name an engine reports through the probe: its kind, its unit, its
/// help. A name not here is said once and measured not at all.
const NAMES: &[(&str, Made, &str, &str)] = &[
    ("tinystore_replication_ack_wait_ms", Made::Timer, "", "A synchronous commit's wait for a copy to hold it."),
    ("tinystore_replication_committed_locally_total", Made::Counter, "commits", "Synchronous commits no copy held."),
    (
        "tinystore_replication_behind_entries",
        Made::Gauge,
        "entries",
        "How far a node copying a file is behind its home.",
    ),
    ("tinystore_jobs_completed_total", Made::Counter, "jobs", "Jobs done."),
    ("tinystore_jobs_failed_total", Made::Counter, "jobs", "Jobs failed for good."),
    ("tinystore_jobs_retried_total", Made::Counter, "jobs", "Attempts that failed and go again."),
    ("tinystore_jobs_run_ms", Made::Timer, "", "A job's run, from its claim to its settlement."),
    ("tinystore_jobs_claim_delay_ms", Made::Timer, "", "How late a claim took a job past its time."),
    ("tinystore_jobs_waiting", Made::Gauge, "jobs", "Jobs waiting in a queue."),
    ("tinystore_jobs_running", Made::Gauge, "jobs", "Jobs a lease holds."),
    ("tinystore_kv_expired_total", Made::Counter, "keys", "Keys deleted once they expired."),
    ("tinystore_kv_cleared_total", Made::Counter, "keys", "Keys deleted after a large clear hid them."),
    ("tinystore_records_events_total", Made::Counter, "events", "Events appended."),
    ("tinystore_records_lines_dropped_total", Made::Counter, "lines", "A logger's lines dropped."),
    ("tinystore_records_segments_packed_total", Made::Counter, "segments", "Segments packed from heads."),
    ("tinystore_records_segments_merged_total", Made::Counter, "segments", "Segments merged into larger ones."),
    ("tinystore_records_expired_total", Made::Counter, "rows", "Head rows and segments past what a stream keeps."),
    ("tinystore_metrics_samples_total", Made::Counter, "samples", "Samples an ingest wrote."),
    ("tinystore_channels_published_total", Made::Counter, "messages", "Messages published."),
    ("tinystore_channels_delivered_total", Made::Counter, "messages", "Messages a subscriber took."),
    (
        "tinystore_channels_missed_total",
        Made::Counter,
        "messages",
        "Messages a subscriber fell too far behind to take.",
    ),
    ("tinystore_locks_wait_ms", Made::Timer, "", "An ask's wait for its key, until it held it or gave up."),
    ("tinystore_locks_busy_total", Made::Counter, "asks", "Asks whose key stayed held past their wait."),
    ("tinystore_locks_lost_total", Made::Counter, "leases", "Leases that ran out or were revoked while held."),
    ("tinystore_search_ms", Made::Timer, "", "A search of an index."),
    ("tinystore_search_head_sets", Made::Gauge, "documents", "Sets an index holds in its head, waiting to be packed."),
    ("tinystore_blobs_written_total", Made::Counter, "files", "Files written."),
    ("tinystore_blobs_written_bytes_total", Made::Counter, "bytes", "The bytes of the files written."),
    ("tinystore_blobs_read_total", Made::Counter, "files", "Files opened to be read."),
    ("tinystore_blobs_read_bytes_total", Made::Counter, "bytes", "The bytes of the files opened to be read."),
    ("tinystore_blobs_damaged_total", Made::Counter, "files", "Contents the scrub found changed or missing."),
    ("tinystore_wire_connections", Made::Gauge, "connections", "Connections open: the pipe's, a server's clients."),
    ("tinystore_wire_requests_total", Made::Counter, "requests", "Requests by their method."),
    ("tinystore_wire_call_ms", Made::Timer, "", "A call answered in one response, by its method."),
];

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
    use std::collections::{HashMap, HashSet};

    use super::{Files, NAMED_DATABASES, NAMED_VALUES, Named, OVERFLOW};

    #[test]
    fn a_labels_values_past_those_named_count_into_the_overflow() {
        let mut named =
            Named { by_name: HashMap::new(), by_set: HashMap::new(), values: HashMap::new(), unknown: HashSet::new() };
        let name = "tinystore_jobs_completed_total";
        for n in 0..NAMED_VALUES {
            assert_eq!(named.bound(name, "queue", &format!("q{n}")), format!("q{n}"));
        }
        assert_eq!(named.bound(name, "queue", "one too many"), "__overflow");
        assert_eq!(named.bound(name, "queue", "q0"), "q0", "a value named stays named");
        assert_eq!(named.bound("tinystore_jobs_failed_total", "queue", "one too many"), "one too many", "a name apart");
    }

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
