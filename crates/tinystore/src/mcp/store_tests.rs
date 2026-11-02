use std::io::Cursor;

use serde_json::{Value, json};

use crate::mcp::{self, Server};
use crate::{Options, Store};

fn held(dir: &std::path::Path) -> Store {
    Store::open(dir, Options { background: false, ..Options::default() }).unwrap()
}

/// What `server` answers to `messages`, a line each.
fn answers(server: Server, messages: &[Value]) -> Vec<Value> {
    let input: String = messages.iter().map(|message| format!("{message}\n")).collect();
    let mut output = Vec::new();
    server.serve_on(Cursor::new(input), &mut output).unwrap();
    String::from_utf8(output).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

/// A tool's answer on `store`'s tools: its JSON, or its error's text.
fn called(store: &Store, name: &str, arguments: Value) -> Result<Value, String> {
    let message = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": name, "arguments": arguments } });
    let read = answers(mcp::server("dashbin", "1.4.0").tools(mcp::read_tools(store)), &[message]);
    let text = read[0]["result"]["content"][0]["text"].as_str().unwrap().to_owned();
    if read[0]["result"]["isError"] == true { Err(text) } else { Ok(serde_json::from_str(&text).unwrap()) }
}

#[cfg(all(feature = "records", feature = "kv", feature = "jobs", feature = "sql", feature = "search"))]
#[test]
fn a_programs_own_tools_and_the_stores_sit_in_one_list_the_stores_under_a_prefix() {
    use crate::mcp::Tool;

    let dir = tempfile::tempdir().unwrap();
    let store = held(dir.path());
    store.bucket::<Value>("sessions").open().unwrap().set("42", &json!({ "user": 42 })).unwrap();
    let api = store.events("api").open().unwrap();
    api.event("log").level(crate::records::Level::WARN).message("slow request").append(&json!({ "ms": 1200 })).unwrap();
    let server = mcp::server("dashbin", "1.4.0").tools(mcp::read_tools(&store).prefix("store_")).tool(Tool::reads(
        "dashboards_list",
        "The project's dashboards.",
        json!({ "type": "object" }),
        |_| Ok(json!([])),
    ));
    let read = answers(
        server,
        &[
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": { "name": "store_kv_get", "arguments": { "bucket": "sessions", "key": "42" } } }),
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": { "name": "store_logs", "arguments": { "level": "warn" } } }),
        ],
    );
    let names: Vec<&str> =
        read[0]["result"]["tools"].as_array().unwrap().iter().map(|tool| tool["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "store_logs",
            "store_kv_get",
            "store_kv_list",
            "store_jobs_list",
            "store_sql_query",
            "store_search_list",
            "dashboards_list"
        ]
    );
    let got: Value = serde_json::from_str(read[1]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(got["value"], json!({ "user": 42 }));
    let lines: Value = serde_json::from_str(read[2]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!((lines[0]["msg"].as_str(), lines[0]["ms"].as_u64()), (Some("slow request"), Some(1200)));
}

// contract: MCP-04
#[cfg(feature = "kv")]
#[test]
fn the_stores_tools_write_nothing_and_see_what_was_committed_before_each_call() {
    let dir = tempfile::tempdir().unwrap();
    let store = held(dir.path());
    let why = called(&store, "kv_get", json!({ "bucket": "sessions", "key": "42" })).unwrap_err();
    assert!(why.contains("holds no kv.db"), "{why}");
    assert!(!dir.path().join("kv.db").exists(), "a read makes no file");

    let sessions = store.bucket::<Value>("sessions").open().unwrap();
    sessions.set("42", &json!({ "user": 42 })).unwrap();
    assert_eq!(called(&store, "kv_get", json!({ "bucket": "sessions", "key": "42" })).unwrap()["value"]["user"], 42);
    sessions.set("43", &json!({ "user": 43 })).unwrap();
    let listed = called(&store, "kv_list", json!({ "bucket": "sessions" })).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 2, "the key set since");

    assert!(called(&store, "kv_get", json!({ "bucket": "sesions", "key": "42" })).is_err());
    let beside = Store::open_beside(dir.path()).unwrap();
    assert!(beside.bucket::<Value>("sesions").open().is_err(), "a misspelt bucket is not made");
}

#[cfg(all(feature = "kv", feature = "jobs", feature = "sql"))]
#[test]
fn a_bucket_and_a_queue_a_program_opened_from_its_database_are_read_with_that_database() {
    let dir = tempfile::tempdir().unwrap();
    let store = held(dir.path());
    let db = store.database("plants").open().unwrap();
    db.bucket::<String>("drafts").open().unwrap().set("fern", &"water weekly".to_owned()).unwrap();
    let queue = db.queue::<Value>("reminders").open().unwrap();
    queue.id("fern-1").delay(std::time::Duration::from_secs(3600)).add(&json!({ "plant": "fern" })).unwrap();

    let why = called(&store, "kv_get", json!({ "bucket": "drafts", "key": "fern" })).unwrap_err();
    assert!(why.contains("read with its database, one of plants"), "{why}");
    let why = called(&store, "jobs_list", json!({ "queue": "reminders" })).unwrap_err();
    assert!(why.contains("holds no jobs.db") && why.contains("one of plants"), "{why}");
    let why = called(&store, "sql_query", json!({ "database": "plnts", "query": "select 1" })).unwrap_err();
    assert!(why.contains("holds no database plnts; did you mean plants?"), "{why}");

    let got = called(&store, "kv_get", json!({ "bucket": "drafts", "key": "fern", "database": "plants" })).unwrap();
    assert_eq!(got["value"], "water weekly");
    let jobs = called(&store, "jobs_list", json!({ "queue": "reminders", "database": "plants" })).unwrap();
    assert_eq!((jobs[0]["id"].as_str(), jobs[0]["state"].as_str()), (Some("fern-1"), Some("scheduled")));
}
