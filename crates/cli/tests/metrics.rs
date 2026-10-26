//! `tinystore metrics` as a shell runs it: a text on stdin, or a stream that
//! does not end, written through the server that holds the directory, or a
//! sidecar the command starts, and refused where no other process can
//! write; and the store's series read, aggregated and deleted.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use tinystore::metrics::Series;
use tinystore::{ErrorKind, Options, Store};

const BINARY: &str = env!("CARGO_BIN_EXE_tinystore");

/// A store's directory, made and let go.
fn made(dir: &Path) {
    Store::open(dir, Options::default()).unwrap().close().unwrap();
}

/// A line of Prometheus text a second before now.
fn rtt(target: &str, value: f64) -> String {
    let at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() - 1000;
    format!("ping_rtt_ms{{target=\"{target}\"}} {value} {at}\n")
}

fn ingest(dir: &Path, text: &str) -> Output {
    let mut command = Command::new(BINARY)
        .arg("metrics")
        .arg(dir)
        .args(["ingest", "--keep", "30d"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    command.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    command.wait_with_output().unwrap()
}

fn said(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

fn stop(dir: &Path) {
    let stopped = Command::new(BINARY).arg("stop").arg(dir).output().unwrap();
    assert!(stopped.status.success(), "{}", said(&stopped));
}

/// The store, once the server that held it has let it go: on Windows a
/// process holds its files until it has exited.
fn opened(dir: &Path) -> Store {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match Store::open(dir, Options::default()) {
            Ok(store) => return store,
            Err(error) if error.kind() == ErrorKind::InUse && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("{error}"),
        }
    }
}

/// The targets of `ping_rtt_ms` in the store, once its server let it go.
fn targets(dir: &Path) -> Vec<String> {
    let store = opened(dir);
    let read =
        store.metrics().unwrap().query().name("ping_rtt_ms").since(Duration::from_secs(3600)).read().unwrap().series;
    let mut targets: Vec<String> = read.iter().map(|series| series.labels()["target"].clone()).collect();
    targets.sort();
    targets
}

// contract: CLI-05
#[test]
fn a_directory_no_one_holds_is_written_through_a_sidecar_the_command_starts() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    made(&data);
    let written = ingest(&data, &(rtt("poland", 23.1) + &rtt("sweden", 31.0)));
    assert!(written.status.success(), "{}", said(&written));
    assert!(said(&written).contains("started a server for"), "{}", said(&written));
    assert!(String::from_utf8_lossy(&written.stdout).starts_with("wrote 2 samples"), "{}", said(&written));
    let again = ingest(&data, &rtt("finland", 40.2));
    assert!(again.status.success(), "{}", said(&again));
    assert!(!said(&again).contains("started a server"), "the second writes through the first's: {}", said(&again));
    stop(&data);
    assert_eq!(targets(&data), ["finland", "poland", "sweden"]);
}

// contract: CLI-05
#[test]
fn commands_that_write_at_once_write_through_one_server() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    made(&data);
    let writers: Vec<_> = ["poland", "sweden", "finland"]
        .into_iter()
        .map(|target| {
            let data = data.clone();
            std::thread::spawn(move || ingest(&data, &rtt(target, 20.0)))
        })
        .collect();
    for writer in writers {
        let written = writer.join().unwrap();
        assert!(written.status.success(), "{}", said(&written));
    }
    stop(&data);
    assert_eq!(targets(&data), ["finland", "poland", "sweden"]);
}

// contract: CLI-05
#[test]
fn a_directory_a_program_holds_inside_its_process_is_refused_at_once() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    let held = Store::open(&data, Options::default()).unwrap();
    let began = Instant::now();
    let refused = ingest(&data, &rtt("poland", 23.1));
    assert_eq!(refused.status.code(), Some(1), "{}", said(&refused));
    assert!(said(&refused).contains("inside its own process"), "{}", said(&refused));
    assert!(began.elapsed() < Duration::from_secs(10), "{:?}", began.elapsed());
    held.close().unwrap();
}

// contract: CLI-05
#[test]
fn a_directory_that_holds_no_store_is_refused_and_left_as_it_was() {
    let dirs = tempfile::tempdir().unwrap();
    let refused = ingest(dirs.path(), &rtt("poland", 23.1));
    assert_eq!(refused.status.code(), Some(1), "{}", said(&refused));
    assert!(said(&refused).contains("no store is there"), "{}", said(&refused));
    assert_eq!(std::fs::read_dir(dirs.path()).unwrap().count(), 0, "nothing made of a typo");
}

/// A store whose `ping_rtt_ms` has three targets, a sample a second for a
/// minute ending a minute ago, and poland silent for fifteen seconds of it.
fn filled(dir: &Path) {
    let store = Store::open(dir, Options::default()).unwrap();
    let start = SystemTime::now() - Duration::from_secs(120);
    let series: Vec<Series> = ["poland", "sweden", "finland"]
        .into_iter()
        .map(|target| {
            let seconds = (0..60).filter(|second| target != "poland" || !(20..35).contains(second));
            seconds.fold(Series::gauge("ping_rtt_ms").label("target", target), |series, second| {
                series.sample(start + Duration::from_secs(second), 20.0 + (second % 5) as f64)
            })
        })
        .collect();
    store.metrics().unwrap().ingest(&series).unwrap();
    store.close().unwrap();
}

fn tinystore(args: &[&dyn AsRef<std::ffi::OsStr>]) -> Output {
    Command::new(BINARY).args(args.iter().map(|arg| arg.as_ref())).output().unwrap()
}

fn json_lines(output: &Output) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(&output.stdout).lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

// contract: CLI-01
#[test]
fn a_metric_is_named_read_and_aggregated_from_a_shell() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    filled(&data);
    let names = tinystore(&[&"metrics", &data, &"names", &"--json"]);
    let named = json_lines(&names);
    assert_eq!((named.len(), &named[0]["name"], &named[0]["series"]), (1, &json!("ping_rtt_ms"), &json!(3)));
    let read = tinystore(&[&"metrics", &data, &"read", &"ping_rtt_ms", &"--where", &"target=poland", &"--json"]);
    let samples = json_lines(&read);
    assert_eq!(samples.len(), 45, "{}", said(&read));
    assert!(samples[0]["time"].as_str().unwrap().ends_with('Z'), "a time in JSON is UTC: {}", samples[0]);
    let person =
        tinystore(&[&"metrics", &data, &"read", &"ping_rtt_ms", &"--where", &"target=poland", &"--gaps", &"5s"]);
    assert!(
        said(&person).contains("· 45 samples") && said(&person).contains("16s without samples"),
        "{}",
        said(&person)
    );
    let most =
        tinystore(&[&"metrics", &data, &"aggregate", &"ping_rtt_ms", &"--op", &"max", &"--by", &"target", &"--json"]);
    let mut maxima: Vec<(String, f64)> = json_lines(&most)
        .iter()
        .map(|row| (row["labels"]["target"].as_str().unwrap().to_owned(), row["value"].as_f64().unwrap()))
        .collect();
    maxima.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(maxima, [("finland".to_owned(), 24.0), ("poland".to_owned(), 24.0), ("sweden".to_owned(), 24.0)]);
    let table = tinystore(&[&"metrics", &data, &"aggregate", &"ping_rtt_ms", &"--op", &"count", &"--by", &"target"]);
    assert!(said(&table).contains("finland") && said(&table).contains("45"), "{}", said(&table));
}

#[test]
fn a_store_without_metrics_says_so_and_a_read_beside_makes_none() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    made(&data);
    let names = tinystore(&[&"metrics", &data, &"names"]);
    assert!(names.status.success() && said(&names).contains("holds no metrics yet"), "{}", said(&names));
    assert!(!data.join("metrics.db").exists(), "a read beside makes no file");
}

// contract: CLI-05
#[test]
fn a_series_is_deleted_by_its_exact_labels_through_a_server() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    filled(&data);
    let partial = tinystore(&[&"metrics", &data, &"delete", &"ping_rtt_ms"]);
    assert!(partial.status.success() && said(&partial).contains("holds no series ping_rtt_ms"), "{}", said(&partial));
    let deleted = tinystore(&[&"metrics", &data, &"delete", &"ping_rtt_ms", &"target=poland"]);
    assert!(said(&deleted).contains(r#"deleted ping_rtt_ms{target="poland"}"#), "{}", said(&deleted));
    stop(&data);
    assert_eq!(targets(&data), ["finland", "sweden"]);
}

// contract: CLI-05
#[test]
fn a_text_past_one_message_is_written_whole() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    made(&data);
    let pad = "p".repeat(80);
    let text: String = (0..30_000).map(|n| format!("wide{{n=\"{n}\",pad=\"{pad}\"}} {n}\n")).collect();
    let written = ingest(&data, &text);
    assert!(written.status.success() && said(&written).contains("wrote 30000 samples"), "{}", said(&written));
    stop(&data);
    let names = opened(&data).metrics().unwrap().names().prefix("wide").page().unwrap().names;
    assert_eq!(names[0].series_count, 30_000);
}

/// The samples of `name` read beside whoever holds the store, once there are
/// `count` of them, or what there are after ten seconds.
fn samples_beside(dir: &Path, name: &str, count: usize) -> usize {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let store = Store::open_beside(dir).unwrap();
        let read = store.metrics().map(|metrics| metrics.query().name(name).since(Duration::from_secs(3600)).read());
        let seen =
            read.ok().and_then(Result::ok).map_or(0, |read| read.series.iter().map(|one| one.samples().len()).sum());
        if seen >= count || Instant::now() > deadline {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// contract: CLI-06
#[test]
fn a_stream_is_written_as_it_comes_and_a_line_it_cannot_read_is_skipped() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    made(&data);
    let mut command = Command::new(BINARY)
        .arg("metrics")
        .arg(&data)
        .args(["ingest", "-f", "--kind", "ping_lost_total=counter"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stream = command.stdin.take().unwrap();
    stream.write_all(b"ping_rtt_ms{target=\"poland\"} 23.1\nping_rtt_ms{target=\"poland\"} time=22.9\n").unwrap();
    stream.flush().unwrap();
    assert_eq!(samples_beside(&data, "ping_rtt_ms", 1), 1, "written while the stream is still open");
    stream.write_all(b"ping_rtt_ms{target=\"poland\"} 24.0\nping_lost_total{target=\"poland\"} 1\n").unwrap();
    drop(stream);
    let followed = command.wait_with_output().unwrap();
    let text = said(&followed);
    assert_eq!(followed.status.code(), Some(1), "a line was skipped: {text}");
    assert!(text.contains("line 2, ") && text.contains("skipped 1 line"), "{text}");
    assert!(text.contains("ping_rtt_ms came with no # TYPE, so it is a gauge"), "{text}");
    assert!(String::from_utf8_lossy(&followed.stdout).contains("wrote 3 samples"), "{text}");
    stop(&data);
    let names = opened(&data).metrics().unwrap().names().page().unwrap().names;
    let kinds: Vec<(&str, &str)> = names.iter().map(|info| (info.name.as_str(), info.kind.as_str())).collect();
    assert_eq!(kinds, [("ping_lost_total", "counter"), ("ping_rtt_ms", "gauge")]);
}
