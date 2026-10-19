use std::time::Duration;

use crate::metrics::{Metrics, SelfMetrics};
use crate::{Options, Store};

const HOUR: Duration = Duration::from_secs(3600);

fn store(dir: &std::path::Path, self_metrics: bool) -> Store {
    let self_metrics = self_metrics.then(SelfMetrics::default);
    Store::open(dir, Options { self_metrics, background: false, ..Options::default() }).unwrap()
}

/// The newest value of `name` under `file`, none when the store holds none.
#[cfg(feature = "kv")]
fn newest(metrics: &Metrics, name: &str, file: &str) -> Option<f64> {
    let latest = metrics.query().name(name).filter("file", file).since(HOUR).latest().unwrap();
    latest.first().map(|latest| latest.value)
}

/// The newest value of the store's own `name`, which has no labels.
#[cfg(any(feature = "blobs", feature = "wire"))]
fn alone(metrics: &Metrics, name: &str) -> Option<f64> {
    metrics.query().name(name).since(HOUR).latest().unwrap().first().map(|latest| latest.value)
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
/// in turns, the median of five passes; what docs/guides/metrics/self-metrics.md's cost asks.
///
/// cargo test --release -p tinystore probe::probe_tests::sets_measured_and_not -- --ignored --nocapture
#[cfg(feature = "kv")]
#[test]
#[ignore = "a timing, not a check"]
fn sets_measured_and_not() {
    use std::time::Instant;
    const SETS: usize = 20_000;
    let rate = |self_metrics: bool, threads: usize| {
        let dir = tempfile::tempdir().unwrap();
        let self_metrics = self_metrics.then(SelfMetrics::default);
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

#[cfg(feature = "jobs")]
#[test]
fn a_queue_is_measured_by_what_its_jobs_did_and_what_waits() {
    let latest = |metrics: &Metrics, name: &str| {
        let latest = metrics.query().name(name).filter("queue", "emails").since(HOUR).latest().unwrap();
        latest.first().map(|latest| latest.value)
    };
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), true);
    let emails = store.queue::<String>("emails").attempts(1).open().unwrap();
    for to in ["ada", "bob", "cy"] {
        emails.add(&to.to_owned()).unwrap();
    }
    emails.claim().unwrap().unwrap().settle(()).unwrap();
    let failing = emails.claim().unwrap().unwrap();
    let outcome = failing.run().fail("no such address");
    failing.settle(outcome).unwrap();
    let metrics = store.metrics().unwrap();
    metrics.flush().unwrap();
    assert_eq!(latest(&metrics, "tinystore_jobs_completed_total"), Some(1.0));
    assert_eq!(latest(&metrics, "tinystore_jobs_failed_total"), Some(1.0));
    assert_eq!(latest(&metrics, "tinystore_jobs_run_ms_count"), Some(2.0));
    assert_eq!(latest(&metrics, "tinystore_jobs_claim_delay_ms_count"), Some(2.0));
    assert_eq!(latest(&metrics, "tinystore_jobs_waiting"), Some(1.0));
    assert_eq!(latest(&metrics, "tinystore_jobs_running"), Some(0.0));
    store.close().unwrap();
}

/// The newest value of the store's own `name` whose label `label` is `value`.
#[cfg(any(feature = "kv", feature = "records", feature = "channels", feature = "locks", feature = "search"))]
fn labelled(metrics: &Metrics, name: &str, label: &str, value: &str) -> Option<f64> {
    let latest = metrics.query().name(name).filter(label, value).since(HOUR).latest().unwrap();
    latest.first().map(|latest| latest.value)
}

#[cfg(feature = "kv")]
#[test]
fn kv_counts_the_keys_it_deleted_once_they_expired() {
    let dir = tempfile::tempdir().unwrap();
    let clock = std::sync::Arc::new(crate::TestClock::new(std::time::SystemTime::now()));
    let options = Options {
        self_metrics: Some(SelfMetrics { system: false }),
        background: false,
        clock: Some(clock.clone()),
        ..Options::default()
    };
    let store = Store::open(dir.path(), options).unwrap();
    let codes = store.bucket::<u32>("codes").ttl(Duration::from_secs(60)).open().unwrap();
    for (key, code) in [("a", 1), ("b", 2), ("c", 3)] {
        codes.set(key, &code).unwrap();
    }
    clock.advance(Duration::from_secs(61));
    crate::kv::maintain(&store).unwrap();
    let metrics = store.metrics().unwrap();
    metrics.flush().unwrap();
    assert_eq!(labelled(&metrics, "tinystore_kv_expired_total", "file", "kv"), Some(3.0));
    store.close().unwrap();
}

#[cfg(feature = "records")]
#[test]
fn records_counts_the_events_each_stream_took() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), true);
    let api = store.events("api").open().unwrap();
    for route in ["/a", "/b", "/c"] {
        api.append("request", &serde_json::json!({ "route": route })).unwrap();
    }
    let metrics = store.metrics().unwrap();
    metrics.flush().unwrap();
    assert_eq!(labelled(&metrics, "tinystore_records_events_total", "stream", "api"), Some(3.0));
    store.close().unwrap();
}

#[cfg(feature = "channels")]
#[test]
fn a_channel_counts_what_was_published_and_what_its_subscribers_took() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), true);
    let chat = store.channel::<String>("chat").open().unwrap();
    let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let kept = std::sync::Arc::clone(&received);
    let sub = chat.subscribe(move |message| kept.lock().unwrap().push(message.value)).unwrap();
    for text in ["a", "b"] {
        chat.publish(&text.to_owned()).unwrap();
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while received.lock().unwrap().len() < 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    sub.stop();
    let metrics = store.metrics().unwrap();
    metrics.flush().unwrap();
    assert_eq!(labelled(&metrics, "tinystore_channels_published_total", "channel", "chat"), Some(2.0));
    assert_eq!(labelled(&metrics, "tinystore_channels_delivered_total", "channel", "chat"), Some(2.0));
    store.close().unwrap();
}

#[cfg(feature = "locks")]
#[test]
fn a_lock_times_each_wait_for_its_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), true);
    let invoices = store.lock("invoices").open().unwrap();
    invoices.hold("42", |_| Ok::<_, crate::Error>(())).unwrap();
    invoices.hold("43", |_| Ok::<_, crate::Error>(())).unwrap();
    let metrics = store.metrics().unwrap();
    metrics.flush().unwrap();
    assert_eq!(labelled(&metrics, "tinystore_locks_wait_ms_count", "lock", "invoices"), Some(2.0));
    store.close().unwrap();
}

#[cfg(feature = "search")]
#[test]
fn an_index_times_its_searches_and_says_what_its_head_holds() {
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Note {
        title: String,
    }
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), true);
    let notes = store.index::<Note>("notes").field("title", 1.0).open().unwrap();
    notes.set("a", &Note { title: "sourdough bread".to_owned() }).unwrap();
    notes.search("bread").run().unwrap();
    let metrics = store.metrics().unwrap();
    metrics.flush().unwrap();
    assert_eq!(labelled(&metrics, "tinystore_search_ms_count", "index", "notes"), Some(1.0));
    assert!(labelled(&metrics, "tinystore_search_head_sets", "index", "notes").is_some_and(|sets| sets >= 1.0));
    store.close().unwrap();
}

#[cfg(feature = "blobs")]
#[test]
fn files_count_what_was_written_and_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), true);
    let docs = store.files("docs").open().unwrap();
    docs.put("a.txt", b"abc").unwrap();
    docs.put("b.txt", b"defgh").unwrap();
    docs.get("a.txt").unwrap().unwrap().read_all().unwrap();
    let metrics = store.metrics().unwrap();
    metrics.flush().unwrap();
    assert_eq!(alone(&metrics, "tinystore_blobs_written_total"), Some(2.0));
    assert_eq!(alone(&metrics, "tinystore_blobs_written_bytes_total"), Some(8.0));
    assert_eq!(alone(&metrics, "tinystore_blobs_read_total"), Some(1.0));
    store.close().unwrap();
}

#[cfg(feature = "wire")]
#[test]
fn a_connection_counts_while_it_is_open() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), true);
    let metrics = store.metrics().unwrap();
    let pipe = crate::pipe::Pipe::connect(&store, crate::pipe::Connect::default()).unwrap();
    metrics.flush().unwrap();
    assert_eq!(alone(&metrics, "tinystore_wire_connections"), Some(1.0));
    drop(pipe);
    metrics.flush().unwrap();
    assert_eq!(alone(&metrics, "tinystore_wire_connections"), Some(0.0));
    store.close().unwrap();
}
