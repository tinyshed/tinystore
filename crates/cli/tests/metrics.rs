//! `tinystore metrics ingest` as a shell runs it: a text on stdin written
//! through the server that holds the directory, or a sidecar the command
//! starts, and refused where no other process can write.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

/// The targets of `ping_rtt_ms` in the store, once the server that held it
/// has let it go: on Windows a process holds its files until it has exited.
fn targets(dir: &Path) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let store = loop {
        match Store::open(dir, Options::default()) {
            Ok(store) => break store,
            Err(error) if error.kind() == ErrorKind::InUse && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("{error}"),
        }
    };
    let read = store.metrics().unwrap().query().name("ping_rtt_ms").since(Duration::from_secs(3600)).read().unwrap();
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
