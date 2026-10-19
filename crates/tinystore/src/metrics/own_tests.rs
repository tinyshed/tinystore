use std::time::{Duration, SystemTime};

use crate::metrics::{Metrics, Series};
use crate::{ErrorKind, Options, Store};

const HOUR: Duration = Duration::from_secs(3600);

fn store(dir: &std::path::Path, self_metrics: bool) -> Store {
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
    // an ingest of a new name commits it, then its samples
    ingest_one(&metrics);
    metrics.flush().unwrap();
    assert_eq!(newest(&metrics, "tinystore_commits_total", "metrics"), Some(2.0));
    assert_eq!(newest(&metrics, "tinystore_commit_writes_total", "metrics"), Some(2.0));
    assert_eq!(newest(&metrics, "tinystore_commit_ms_count", "metrics"), Some(2.0));
    assert_eq!(newest(&metrics, "tinystore_writer_wait_ms_count", "metrics"), Some(2.0));
    // each flush wrote the totals: commits of metrics.db that nothing counts
    metrics.flush().unwrap();
    metrics.flush().unwrap();
    assert_eq!(newest(&metrics, "tinystore_commits_total", "metrics"), Some(2.0));
    ingest_one(&metrics);
    metrics.flush().unwrap();
    assert_eq!(newest(&metrics, "tinystore_commits_total", "metrics"), Some(3.0));
    assert!(newest(&metrics, "tinystore_file_bytes", "metrics").is_some_and(|bytes| bytes > 0.0));
    assert!(newest(&metrics, "tinystore_readers_open", "metrics").is_some());
    let used = metrics.query().name("tinystore_memory_used_bytes").since(HOUR).latest().unwrap();
    assert_eq!(used.len(), 1);
    store.close().unwrap();
}

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
    // the close wrote the first run's totals, the name and the samples; the new run starts from a zero
    assert_eq!(newest(&metrics, "tinystore_commits_total", "metrics"), Some(2.0));
    ingest_one(&metrics);
    metrics.flush().unwrap();
    let read = metrics.query().name("tinystore_commits_total").filter("file", "metrics").since(HOUR).read().unwrap();
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

#[cfg(feature = "kv")]
#[test]
fn every_file_is_measured_by_its_own_label() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), true);
    let sessions = store.bucket::<String>("sessions").open().unwrap();
    sessions.set("a", &"x".to_owned()).unwrap();
    sessions.set("b", &"y".to_owned()).unwrap();
    let metrics = store.metrics().unwrap();
    metrics.flush().unwrap();
    assert!(newest(&metrics, "tinystore_commits_total", "kv").is_some_and(|commits| commits >= 2.0));
    assert!(newest(&metrics, "tinystore_writer_wait_ms_count", "kv").is_some_and(|waits| waits >= 2.0));
    store.close().unwrap();
}

/// kv's sets a second from 1, 8 and 64 threads, the store measured and not,
/// in turns, the median of five passes; what plan/self-metrics.md's cost asks.
///
/// cargo test --release -p tinystore metrics::own::own_tests::sets_measured_and_not -- --ignored --nocapture
#[cfg(feature = "kv")]
#[test]
#[ignore = "a timing, not a check"]
fn sets_measured_and_not() {
    use std::time::Instant;
    const SETS: usize = 20_000;
    let rate = |self_metrics: bool, threads: usize| {
        let dir = tempfile::tempdir().unwrap();
        let options = Options { self_metrics, durability: Some(crate::Durability::Os), ..Options::default() };
        let store = Store::open(dir.path(), options).unwrap();
        let bucket = store.bucket::<u64>("counts").open().unwrap();
        let started = Instant::now();
        std::thread::scope(|scope| {
            for thread in 0..threads {
                let bucket = &bucket;
                scope.spawn(move || {
                    for n in 0..SETS / threads {
                        bucket.set(format!("{thread}-{n}"), &(n as u64)).unwrap();
                    }
                });
            }
        });
        let rate = SETS as f64 / started.elapsed().as_secs_f64();
        store.close().unwrap();
        rate
    };
    for threads in [1, 8, 64] {
        let (mut off, mut on) = (Vec::new(), Vec::new());
        for _ in 0..5 {
            off.push(rate(false, threads));
            on.push(rate(true, threads));
        }
        off.sort_by(f64::total_cmp);
        on.sort_by(f64::total_cmp);
        let (off, on) = (off[2], on[2]);
        println!("{threads:>2} threads: off {off:>9.0}/s, on {on:>9.0}/s, {:+.1} %", (on / off - 1.0) * 100.0);
    }
}
