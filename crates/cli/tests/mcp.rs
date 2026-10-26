//! `tinystore mcp --write` as an agent's host runs it: the tools that write
//! offered only as the flag names them, each marked as writing, and a write
//! through the server that holds the directory, or a sidecar it starts.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tinystore::{ErrorKind, Options, Store};

const BINARY: &str = env!("CARGO_BIN_EXE_tinystore");

fn said(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// The answers of `tinystore mcp` to `messages`, one a line.
fn answers(dir: &Path, flags: &[&str], messages: &[Value]) -> Vec<Value> {
    let mut mcp = Command::new(BINARY)
        .arg("mcp")
        .arg(dir)
        .args(flags)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = mcp.stdin.take().unwrap();
    let lines: String = messages.iter().map(|message| format!("{message}\n")).collect();
    stdin.write_all(lines.as_bytes()).unwrap();
    drop(stdin);
    let read: Vec<Value> =
        BufReader::new(mcp.stdout.take().unwrap()).lines().map(|line| line.unwrap().parse().unwrap()).collect();
    mcp.wait().unwrap();
    read
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

// contract: CLI-05
#[test]
fn an_agent_writes_with_the_tools_its_host_was_given_and_no_other() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    Store::open(&data, Options::default()).unwrap().close().unwrap();
    let set = json!({ "name": "kv_set", "arguments": { "bucket": "flags", "key": "beta", "value": "on" } });
    let delete = json!({ "name": "kv_delete", "arguments": { "bucket": "flags", "key": "beta" } });
    let read = answers(
        &data,
        &["--write", "kv_set"],
        &[request(1, "tools/list", json!({})), request(2, "tools/call", set), request(3, "tools/call", delete)],
    );
    let tools = read[0]["result"]["tools"].as_array().unwrap();
    let named = |name: &str| tools.iter().find(|tool| tool["name"] == name);
    assert_eq!(named("kv_set").unwrap()["annotations"]["readOnlyHint"], false);
    assert_eq!(named("kv_get").unwrap()["annotations"]["readOnlyHint"], true);
    assert!(named("kv_delete").is_none(), "a tool --write does not name is not offered");
    assert_eq!(read[1]["result"]["content"][0]["text"], r#"{"set":true}"#, "{}", read[1]);
    assert_eq!(read[2]["result"]["isError"], true, "{}", read[2]);
    assert!(read[2]["result"]["content"][0]["text"].as_str().unwrap().contains("--write kv_delete"), "{}", read[2]);

    let misspelt = Command::new(BINARY).arg("mcp").arg(&data).args(["--write", "kv_sett"]).output().unwrap();
    assert_eq!(misspelt.status.code(), Some(2), "{}", said(&misspelt));
    assert!(said(&misspelt).contains("did you mean kv_set"), "{}", said(&misspelt));

    let stop = Command::new(BINARY).arg("stop").arg(&data).output().unwrap();
    assert!(stop.status.success(), "{}", said(&stop));
    let deadline = Instant::now() + Duration::from_secs(10);
    let store = loop {
        match Store::open(&data, Options::default()) {
            Ok(store) => break store,
            Err(error) if error.kind() == ErrorKind::InUse && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("{error}"),
        }
    };
    assert_eq!(store.bucket::<String>("flags").open().unwrap().get("beta").unwrap(), Some("on".to_owned()));
}
