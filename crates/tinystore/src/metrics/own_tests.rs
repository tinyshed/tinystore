use std::time::{Duration, SystemTime};

use crate::metrics::{Metrics, SelfMetrics, Series};
use crate::{ErrorKind, Options, Store};

const HOUR: Duration = Duration::from_secs(3600);

fn store(dir: &std::path::Path, self_metrics: bool) -> Store {
    let self_metrics = self_metrics.then(SelfMetrics::default);
    Store::open(dir, Options { self_metrics, background: false, ..Options::default() }).unwrap()
}

/// The newest value of `name` under `file`, none when the store holds none.
fn newest(metrics: &Metrics, name: &str, file: &str) -> Option<f64> {
    let latest = metrics.query().name(name).filter("file", file).since(HOUR).latest().unwrap();
    latest.first().map(|latest| latest.value)
}

fn ingest_one(metrics: &Metrics) {
    let cpu = Series::gauge("cpu").label("host", "web-1").sample(SystemTime::now(), 0.42);
    metrics.ingest(&[cpu]).unwrap();
}

#[test]
fn a_store_measured_counts_its_commits_and_its_own_writes_of_them_count_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), true);
    let metrics = store.metrics().unwrap();
    // an ingest of a new name commits it with its samples
    ingest_one(&metrics);
    metrics.flush().unwrap();
    assert_eq!(newest(&metrics, "tinystore_commits_total", "metrics"), Some(1.0));
    assert_eq!(newest(&metrics, "tinystore_commit_writes_total", "metrics"), Some(1.0));
    assert_eq!(newest(&metrics, "tinystore_commit_ms_count", "metrics"), Some(1.0));
    assert_eq!(newest(&metrics, "tinystore_writer_wait_ms_count", "metrics"), Some(1.0));
    // each flush wrote the totals: commits of metrics.db that nothing counts
    metrics.flush().unwrap();
    metrics.flush().unwrap();
    assert_eq!(newest(&metrics, "tinystore_commits_total", "metrics"), Some(1.0));
    ingest_one(&metrics);
    metrics.flush().unwrap();
    assert_eq!(newest(&metrics, "tinystore_commits_total", "metrics"), Some(2.0));
    assert!(newest(&metrics, "tinystore_file_bytes", "metrics").is_some_and(|bytes| bytes > 0.0));
    assert!(newest(&metrics, "tinystore_readers_open", "metrics").is_some());
    let used = metrics.query().name("tinystore_memory_used_bytes").since(HOUR).latest().unwrap();
    assert_eq!(used.len(), 1);
    store.close().unwrap();
}

// contract: MET-14
#[test]
fn a_store_not_measured_holds_none_of_its_own_names() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), false);
    let metrics = store.metrics().unwrap();
    ingest_one(&metrics);
    metrics.flush().unwrap();
    assert_eq!(newest(&metrics, "tinystore_commits_total", "metrics"), None);
    store.close().unwrap();
}

// contract: MET-02
#[test]
fn a_name_of_the_stores_own_is_refused_from_the_program() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), true);
    let metrics = store.metrics().unwrap();
    let refused = metrics.counter("tinystore_commits_total").open().unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid);
    let series = Series::counter("tinystore_mine").sample(SystemTime::now(), 1.0);
    assert_eq!(metrics.ingest(&[series]).unwrap_err().kind(), ErrorKind::Invalid);
    store.close().unwrap();
}

#[test]
fn what_the_store_measured_is_kept_through_its_close_and_counted_on_after() {
    let dir = tempfile::tempdir().unwrap();
    let first = store(dir.path(), true);
    ingest_one(&first.metrics().unwrap());
    first.close().unwrap();

    let again = store(dir.path(), true);
    let metrics = again.metrics().unwrap();
    // the close wrote the first run's totals, the name with the samples; the new run starts from a zero
    assert_eq!(newest(&metrics, "tinystore_commits_total", "metrics"), Some(1.0));
    ingest_one(&metrics);
    metrics.flush().unwrap();
    let read =
        metrics.query().name("tinystore_commits_total").filter("file", "metrics").since(HOUR).read().unwrap().series;
    let values: Vec<f64> = read[0].samples().iter().map(|sample| sample.value).collect();
    assert_eq!(values.first(), Some(&0.0));
    assert_eq!(values.last(), Some(&1.0));
    assert!(values.contains(&0.0) && values.iter().filter(|value| **value == 0.0).count() == 2, "{values:?}");
    again.close().unwrap();
}

#[test]
fn a_failure_of_periodic_work_is_counted_by_its_task() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), true);
    let metrics = store.metrics().unwrap();
    store.probe().failed("records: packing");
    store.probe().failed("records: packing");
    metrics.flush().unwrap();
    let latest =
        metrics.query().name("tinystore_background_failures_total").filter("task", "records: packing").since(HOUR);
    assert_eq!(latest.latest().unwrap()[0].value, 2.0);
    store.close().unwrap();
}

#[cfg(feature = "system")]
#[test]
fn the_machine_is_measured_unless_left_out() {
    let latest = |metrics: &Metrics, name: &str| {
        metrics.query().name(name).since(HOUR).latest().unwrap().first().map(|latest| latest.value)
    };
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), true);
    let metrics = store.metrics().unwrap();
    metrics.flush().unwrap();
    assert!(latest(&metrics, "tinystore_system_memory_bytes").is_some_and(|bytes| bytes > 0.0));
    assert!(latest(&metrics, "tinystore_process_memory_bytes").is_some_and(|bytes| bytes > 0.0));
    assert!(latest(&metrics, "tinystore_system_cpus").is_some_and(|cores| cores >= 1.0));
    // a processor's use is counted from the read before
    assert_eq!(latest(&metrics, "tinystore_system_cpu_percent"), None);
    metrics.flush().unwrap();
    assert!(latest(&metrics, "tinystore_system_cpu_percent").is_some());
    let cores = metrics.query().name("tinystore_system_core_cpu_percent").since(HOUR).latest().unwrap();
    assert_eq!(cores.len() as f64, latest(&metrics, "tinystore_system_cpus").unwrap());
    store.close().unwrap();

    let dir = tempfile::tempdir().unwrap();
    let options =
        Options { self_metrics: Some(SelfMetrics { system: false }), background: false, ..Options::default() };
    let store = Store::open(dir.path(), options).unwrap();
    let metrics = store.metrics().unwrap();
    metrics.flush().unwrap();
    metrics.flush().unwrap();
    assert_eq!(latest(&metrics, "tinystore_system_memory_bytes"), None);
    assert_eq!(latest(&metrics, "tinystore_process_cpu_percent"), None);
    assert!(latest(&metrics, "tinystore_memory_used_bytes").is_some(), "the store's own still are");
    store.close().unwrap();
}

/// The newest value of the store's own `name`, which has no labels.
fn alone(metrics: &Metrics, name: &str) -> Option<f64> {
    metrics.query().name(name).since(HOUR).latest().unwrap().first().map(|latest| latest.value)
}

#[test]
fn an_ingest_counts_its_samples() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), true);
    let metrics = store.metrics().unwrap();
    let now = SystemTime::now();
    let cpu = Series::gauge("cpu").sample(now - Duration::from_secs(2), 0.1).sample(now, 0.2);
    metrics.ingest(&[cpu]).unwrap();
    metrics.flush().unwrap();
    assert_eq!(alone(&metrics, "tinystore_metrics_samples_total"), Some(2.0));
    store.close().unwrap();
}
