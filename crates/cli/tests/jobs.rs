//! `tinystore jobs` as a shell runs it: a job added and cancelled through
//! the server that holds the directory, or a sidecar the command starts, and
//! a queue listed beside it.

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::Value;
use tinystore::jobs::State;
use tinystore::{ErrorKind, Options, Store};

const BINARY: &str = env!("CARGO_BIN_EXE_tinystore");

fn tinystore(args: &[&dyn AsRef<std::ffi::OsStr>]) -> Output {
    Command::new(BINARY).args(args).output().unwrap()
}

fn said(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// The store once the server that held it has let it go.
fn stopped(dir: &Path) -> Store {
    let stop = tinystore(&[&"stop", &dir]);
    assert!(stop.status.success(), "{}", said(&stop));
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

// contract: CLI-05
#[test]
fn a_job_is_added_listed_and_cancelled_from_a_shell() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    Store::open(&data, Options::default()).unwrap().close().unwrap();
    let welcome = r#"{"to": "ada@example.com"}"#;
    let added = tinystore(&[&"jobs", &data, &"add", &"emails", &welcome, &"--id", &"welcome-42", &"--delay", &"1h"]);
    assert!(said(&added).contains("added welcome-42 to queue emails, due in 1h"), "{}", said(&added));
    let again = tinystore(&[&"jobs", &data, &"add", &"emails", &welcome, &"--id", &"welcome-42"]);
    assert!(said(&again).contains("holds a job welcome-42 already; nothing added"), "{}", said(&again));
    let unnamed = tinystore(&[&"jobs", &data, &"add", &"emails", &r#"{"to": "bob@example.com"}"#, &"--delay", &"1h"]);
    assert!(said(&unnamed).contains("a job without --id cannot be cancelled"), "{}", said(&unnamed));
    let wrong = tinystore(&[&"jobs", &data, &"add", &"emails", &"{to: ada}"]);
    assert_eq!(wrong.status.code(), Some(2), "a job is JSON: {}", said(&wrong));
    let listed = tinystore(&[&"jobs", &data, &"list", &"emails", &"--json"]);
    let jobs: Vec<Value> = String::from_utf8_lossy(&listed.stdout).lines().map(|line| line.parse().unwrap()).collect();
    assert_eq!(jobs.len(), 1, "a job without an id is listed by none: {}", said(&listed));
    assert_eq!((jobs[0]["id"].as_str(), jobs[0]["state"].as_str()), (Some("welcome-42"), Some("scheduled")));
    assert_eq!(jobs[0]["value"]["to"], "ada@example.com");
    let cancelled = tinystore(&[&"jobs", &data, &"cancel", &"emails", &"welcome-42"]);
    assert!(said(&cancelled).contains("cancelled welcome-42 in queue emails"), "{}", said(&cancelled));
    let store = stopped(&data);
    let queue = store.queue::<Value>("emails").open().unwrap();
    assert!(queue.get("welcome-42").unwrap().is_none());
    let waiting = queue.list(&tinystore::jobs::Filter::default().state(State::Scheduled)).unwrap();
    assert!(waiting.jobs.is_empty(), "a page lists the jobs that have ids");
}
