//! `tinystore events` as a shell runs it: an event given in words or another
//! program's output on stdin, written through a server, and a stream listed
//! beside whoever holds the store.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use tinystore::{ErrorKind, Options, Store};

const BINARY: &str = env!("CARGO_BIN_EXE_tinystore");

fn made(dir: &Path) {
    Store::open(dir, Options::default()).unwrap().close().unwrap();
}

fn tinystore(args: &[&dyn AsRef<std::ffi::OsStr>], stdin: Option<&str>) -> Output {
    let mut command = Command::new(BINARY)
        .args(args.iter().map(|arg| arg.as_ref()))
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(text) = stdin {
        command.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    }
    command.wait_with_output().unwrap()
}

fn said(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// The store once the server that held it has let it go: on Windows a process
/// holds its files until it has exited.
fn let_go(dir: &Path) -> Store {
    let stopped = tinystore(&[&"stop", &dir], None);
    assert!(stopped.status.success(), "{}", said(&stopped));
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

fn listed(store: &Store, stream: &str) -> Vec<tinystore::records::Event> {
    store.events(stream).open().unwrap().list().since(Duration::from_secs(3600)).page().unwrap().events
}

// contract: CLI-05
#[test]
fn an_event_given_in_words_is_written_once_by_its_id_its_fields_typed_as_said() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    made(&data);
    let words: [&dyn AsRef<std::ffi::OsStr>; 9] =
        [&"events", &data, &"append", &"audit", &"role.changed", &"user:=42", &"zip=02134", &"--id", &"d-1"];
    let first = tinystore(&words, None);
    assert!(said(&first).contains("appended role.changed to audit"), "{}", said(&first));
    let again = tinystore(&words, None);
    assert!(said(&again).contains("audit holds the id d-1: nothing written"), "{}", said(&again));
    let store = let_go(&data);
    let events = listed(&store, "audit");
    assert_eq!(events.len(), 1);
    let fields: serde_json::Value = events[0].fields.to().unwrap();
    assert_eq!(fields, json!({ "user": 42, "zip": "02134" }), "a number when it says JSON, text otherwise");
}

// contract: CLI-05
#[test]
fn another_programs_output_on_stdin_becomes_a_line_an_event() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    made(&data);
    let output = "level=warn msg=\"slow request\" ms=1200\n{\"level\":30,\"msg\":\"ok\"}\nthe last line";
    let written = tinystore(&[&"events", &data, &"append", &"worker"], Some(output));
    assert!(said(&written).contains("appended 3 lines to worker"), "{}", said(&written));
    let store = let_go(&data);
    let names: Vec<String> = listed(&store, "worker").into_iter().map(|event| event.name).collect();
    assert_eq!(names, ["logfmt", "json", "log"]);
    let refused = tinystore(&[&"events", &data, &"append", &"worker", &"--id", &"d-1"], Some(output));
    assert_eq!(refused.status.code(), Some(2), "an id names one event, not a stream of lines: {}", said(&refused));
}

// contract: CLI-01
#[test]
fn a_stream_is_listed_beside_its_holder_and_a_missing_one_is_not_made() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    let held = Store::open(&data, Options::default()).unwrap();
    let audit = held.events("audit").open().unwrap();
    audit.append("role.changed", &json!({ "user": 42, "role": "admin" })).unwrap();
    audit.append("role.changed", &json!({ "user": 7, "role": "viewer" })).unwrap();
    let number = tinystore(&[&"events", &data, &"list", &"audit", &"--where", &"user:=42", &"--json"], None);
    let lines: Vec<serde_json::Value> =
        String::from_utf8_lossy(&number.stdout).lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(lines.len(), 1, "{}", said(&number));
    assert_eq!((&lines[0]["user"], &lines[0]["role"]), (&json!(42), &json!("admin")));
    let text = tinystore(&[&"events", &data, &"list", &"audit", &"--where", &"user=42"], None);
    assert!(said(&text).contains("no events of audit"), "text 42 is not the number 42: {}", said(&text));
    let missing = tinystore(&[&"events", &data, &"list", &"nothing"], None);
    assert!(missing.status.success() && said(&missing).contains("holds no stream nothing"), "{}", said(&missing));
    let streams: Vec<String> =
        held.all_events().unwrap().streams().unwrap().into_iter().map(|held| held.name).collect();
    assert_eq!(streams, ["audit"], "a read beside made no stream");
    held.close().unwrap();
}
