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

/// JSON lines as a command printed them.
fn json_lines(text: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(text).lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

// contract: CLI-01, CLI-09
#[test]
fn an_export_writes_every_event_of_the_range_as_list_prints_each_a_page_at_a_time() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    let held = Store::open(&data, Options::default()).unwrap();
    let audit = held.events("audit").open().unwrap();
    // past two of the engine's pages, so that the export reads on twice
    let start = std::time::SystemTime::now() - Duration::from_secs(600);
    let events = (0..25_000_u32).map(|number| {
        let at = start + Duration::from_millis(u64::from(number) * 10);
        tinystore::records::NewEvent::new("checked").at(at).fields(&json!({ "number": number, "odd": number % 2 }))
    });
    audit.append_all(events).unwrap();
    let all = tinystore(&[&"events", &data, &"export", &"audit"], None);
    assert!(all.status.success(), "{}", said(&all));
    let exported = json_lines(&all.stdout);
    let numbers: Vec<u64> = exported.iter().map(|line| line["number"].as_u64().unwrap()).collect();
    assert_eq!(numbers, (0..25_000).collect::<Vec<u64>>(), "every event once, oldest first");
    assert!(String::from_utf8_lossy(&all.stderr).contains("wrote 25000 events of audit"), "{}", said(&all));
    let page = tinystore(&[&"events", &data, &"list", &"audit", &"--since", &"1h", &"--limit", &"3", &"--json"], None);
    assert_eq!(json_lines(&page.stdout)[..3], exported[..3], "each event as list --json prints it");
    let file = dirs.path().join("odd.jsonl");
    let odd = tinystore(&[&"events", &data, &"export", &"audit", &"--where", &"odd:=1", &"-o", &file], None);
    assert!(odd.status.success() && odd.stdout.is_empty(), "{}", said(&odd));
    assert_eq!(json_lines(&std::fs::read(&file).unwrap()).len(), 12_500);
    let before = [&"events" as &dyn AsRef<std::ffi::OsStr>, &data, &"export", &"audit", &"--to", &"2000-01-01T00:00Z"];
    let none = tinystore(&before, None);
    assert!(none.status.success() && none.stdout.is_empty(), "{}", said(&none));
    assert!(said(&none).contains("wrote 0 events of audit"), "{}", said(&none));
    held.close().unwrap();
}

// contract: CLI-09
#[test]
fn a_page_past_what_the_engine_holds_is_refused_naming_the_bound_and_export() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    made(&data);
    let refused = tinystore(&[&"events", &data, &"list", &"audit", &"--limit", &"20000"], None);
    assert_eq!(refused.status.code(), Some(2), "{}", said(&refused));
    let why = said(&refused);
    assert!(why.contains("--limit 20000: a page holds 10000 events at most") && why.contains("events export"), "{why}");
}
