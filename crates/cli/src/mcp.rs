//! `tinystore mcp`: the Model Context Protocol on stdin and stdout, a JSON-RPC
//! message a line, with tools that only read a store, beside the program or
//! server that holds it. Each call opens the store again, so it sees what was
//! committed since the last.
//!
//! ```text
//! claude mcp add tinystore -- tinystore mcp ./data
//! ```

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::ExitCode;

use serde_json::{Value, json};
use tinystore::Store;
use tinystore::jobs::{Filter, State};

use crate::asked::{self, Asked, Done, wrong};
use crate::help;
use crate::kv::Reading;

/// The protocol's version this server speaks when a client names none.
const PROTOCOL: &str = "2025-06-18";
/// What a list answers at most, so that an answer fits an agent's context.
const MOST: usize = 200;

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run(&help::MCP, args, serve)
}

fn serve(asked: Asked) -> Done {
    let (dir, rest) = asked.dir_or_here(0)?;
    if !rest.is_empty() {
        return Err(wrong(format!("one directory, not {} too", rest.join(" "))));
    }
    let dir = std::path::absolute(&dir).unwrap_or(dir);
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(asked::failed)?;
        if line.trim().is_empty() {
            continue;
        }
        let Some(answer) = answer(&dir, &line) else { continue };
        writeln!(stdout, "{answer}").and_then(|()| stdout.flush()).map_err(asked::unwritten)?;
    }
    Ok(())
}

/// The answer to one message; none to a notification.
fn answer(dir: &Path, line: &str) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(message) => message,
        Err(error) => return Some(failure(Value::Null, -32700, &format!("not JSON: {error}"))),
    };
    let id = message.get("id").cloned()?;
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match message.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "tinystore", "version": env!("CARGO_PKG_VERSION") },
                "instructions": format!(
                    "Read the TinyStore store in {}: its status, logs, kv values, jobs, SQL databases and search \
                     indexes. Nothing here writes.",
                    dir.display()
                ),
            })
        }
        Some("ping") => json!({}),
        Some("tools/list") => json!({ "tools": tools() }),
        Some("tools/call") => call(dir, &params),
        Some(other) => return Some(failure(id, -32601, &format!("no method {other}"))),
        None => return Some(failure(id, -32600, "a request names its method")),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn failure(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// The tools, each with what it takes.
fn tools() -> Value {
    Value::Array([store_tools(), data_tools()].concat())
}

fn described(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required },
    })
}

fn text(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

/// A bucket's or a queue's database: a program that opened one from a
/// database keeps it in that database's file, not in kv.db or jobs.db.
fn database() -> Value {
    text("the database the program opened it from, as app, when it is kept there rather than in kv.db or jobs.db")
}

fn count(description: &str) -> Value {
    json!({ "type": "integer", "description": description })
}

/// The tools that say how the store stands and what it logged.
fn store_tools() -> Vec<Value> {
    vec![
        described(
            "status",
            "What the store holds: its files and their sizes, the server that serves it, and how its node stands.",
            json!({}),
            &[],
        ),
        described(
            "logs",
            "The application's latest log lines and events, newest last.",
            json!({
                "since": text("only lines from this long ago, as 30s, 15m, 1h or 2d"),
                "level": text("trace, debug, info, warn or error, and above"),
                "grep": text("only lines that hold this text, in any case"),
                "stream": text("one stream, as api"),
                "limit": count("how many of the last lines; 50 by default, 200 at most"),
            }),
            &[],
        ),
    ]
}

/// The tools that read what the application keeps.
fn data_tools() -> Vec<Value> {
    vec![
        described(
            "kv_get",
            "One value of a kv bucket by its key.",
            json!({ "bucket": text("the bucket"), "key": text("the key"), "database": database() }),
            &["bucket", "key"],
        ),
        described(
            "kv_list",
            "A kv bucket's keys and values, in key order.",
            json!({
                "bucket": text("the bucket"),
                "branch": text("the keys under this branch, as 42 or 42/devices"),
                "database": database(),
                "limit": count("200 at most"),
            }),
            &["bucket"],
        ),
        described(
            "jobs_list",
            "A queue's jobs that have ids, in the order of their ids, with their state, attempts and last error; \
             or, with state failed and no prefix, its failed jobs, the last failed first.",
            json!({
                "queue": text("the queue"),
                "prefix": text("the jobs whose ids start with it"),
                "state": text("scheduled, waiting, running, done or failed"),
                "database": database(),
                "limit": count("200 at most"),
            }),
            &["queue"],
        ),
        described(
            "sql_query",
            "Rows of a read-only SQL query on one of the application's databases, sql/<database>.db.",
            json!({
                "database": text("the database, as app"),
                "query": text("one statement that reads, as select count(*) from users"),
            }),
            &["database", "query"],
        ),
        described("search_list", "The store's search indexes, with their documents.", json!({}), &[]),
    ]
}

/// A tool's answer: its text, or what went wrong as the tool's own error.
fn call(dir: &Path, params: &Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    match tool(dir, name, &arguments) {
        Ok(answer) => json!({ "content": [{ "type": "text", "text": answer.to_string() }] }),
        Err(why) => json!({ "content": [{ "type": "text", "text": why }], "isError": true }),
    }
}

fn tool(dir: &Path, name: &str, arguments: &Value) -> Result<Value, String> {
    let text = |key: &str| arguments.get(key).and_then(Value::as_str);
    let needed = |key: &str| text(key).ok_or_else(|| format!("{name} takes {key}"));
    let limit = |default: usize| {
        arguments.get("limit").and_then(Value::as_u64).map_or(default, |limit| limit as usize).min(MOST)
    };
    if name == "status" {
        return crate::status::json_of(dir).map_err(|refused| refused.to_string());
    }
    let store = Store::open_beside(dir).map_err(|error| error.to_string())?;
    match name {
        "logs" => logs(&store, text("since"), text("level"), text("grep"), text("stream"), limit(50)),
        "kv_get" => {
            let (bucket, key) = (needed("bucket")?, needed("key")?);
            let reading = Reading { bucket, database: text("database"), key: Some(key), branch: None };
            let found = crate::kv::entries(&store, &reading, 1)?;
            found.into_iter().next().ok_or_else(|| format!("kv bucket {bucket}: no key {key}"))
        }
        "kv_list" => {
            let reading =
                Reading { bucket: needed("bucket")?, database: text("database"), key: None, branch: text("branch") };
            Ok(Value::Array(crate::kv::entries(&store, &reading, limit(MOST))?))
        }
        "jobs_list" => {
            let listing = Listing { queue: needed("queue")?, database: text("database"), prefix: text("prefix") };
            jobs(&store, &listing, text("state"), limit(MOST))
        }
        "sql_query" => crate::sql::rows_json(&store, needed("database")?, needed("query")?, MOST),
        "search_list" => {
            let listed = tinystore::search::list(&store).map_err(|error| error.to_string())?;
            let shown: Vec<Value> = listed
                .iter()
                .map(|index| json!({ "name": index.name, "documents": index.documents, "vectors": index.vectors }))
                .collect();
            Ok(Value::Array(shown))
        }
        other => Err(format!("no tool {other}")),
    }
}

fn logs(
    store: &Store,
    since: Option<&str>,
    level: Option<&str>,
    grep: Option<&str>,
    stream: Option<&str>,
    limit: usize,
) -> Result<Value, String> {
    let events = store.all_events().map_err(|error| error.to_string())?;
    let mut list = events.list().newest().limit(limit);
    if let Some(since) = since {
        list = list.since(asked::span(since)?);
    }
    if let Some(level) = level {
        let level = crate::logs::level_named(level)
            .ok_or_else(|| format!("no level {level}: trace, debug, info, warn or error"))?;
        list = list.min_level(level);
    }
    if let Some(grep) = grep {
        list = list.search(grep);
    }
    if let Some(stream) = stream {
        list = list.streams([stream]);
    }
    let mut page = list.page().map_err(|error| error.to_string())?.events;
    page.reverse();
    let lines: Vec<Value> =
        page.iter().map(|event| serde_json::from_str(&crate::logs::json(event)).unwrap_or(Value::Null)).collect();
    Ok(Value::Array(lines))
}

/// The queue `jobs_list` reads, and the jobs of it by their ids.
struct Listing<'a> {
    queue: &'a str,
    database: Option<&'a str>,
    prefix: Option<&'a str>,
}

fn jobs(store: &Store, listing: &Listing, state: Option<&str>, limit: usize) -> Result<Value, String> {
    let opened = match listing.database {
        Some(database) => {
            crate::sql::opened(store, database).map_err(|refused| refused.to_string())?.queue(listing.queue)
        }
        None if !store.dir().join("jobs.db").exists() => {
            return Err(crate::files::none_kept(store.dir(), "jobs.db", "queue"));
        }
        None => store.queue::<Value>(listing.queue),
    };
    let opened = opened.open().map_err(|error| error.to_string())?;
    let mut filter = Filter::prefix(listing.prefix.unwrap_or_default()).limit(limit);
    if let Some(state) = state {
        filter = filter.state(match state {
            "scheduled" => State::Scheduled,
            "waiting" => State::Waiting,
            "running" => State::Running,
            "done" => State::Done,
            "failed" => State::Failed,
            other => return Err(format!("no state {other}: scheduled, waiting, running, done or failed")),
        });
    }
    let page = opened.list(&filter).map_err(|error| error.to_string())?;
    let shown: Vec<Value> = page
        .jobs
        .iter()
        .map(|job| {
            json!({
                "id": job.id,
                "state": format!("{:?}", job.state).to_lowercase(),
                "at": jiff::Timestamp::try_from(job.at).map(|at| at.to_string()).ok(),
                "attempt": job.attempt,
                "error": job.error,
                "group": job.group,
                "value": job.value,
            })
        })
        .collect();
    Ok(Value::Array(shown))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tinystore::Options;

    fn asked(dir: &Path, message: Value) -> Value {
        answer(dir, &message.to_string()).unwrap()
    }

    #[test]
    fn an_agent_lists_the_tools_and_reads_a_value_beside_the_program_that_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        store.bucket::<serde_json::Value>("sessions").open().unwrap().set("42", &json!({ "user": 42 })).unwrap();
        let started = asked(dir.path(), json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }));
        assert_eq!(started["result"]["serverInfo"]["name"], "tinystore");
        assert!(
            answer(dir.path(), &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string())
                .is_none()
        );
        let listed = asked(dir.path(), json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
        assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 7);
        let called = json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "kv_get", "arguments": { "bucket": "sessions", "key": "42" } } });
        let got = asked(dir.path(), called);
        let text = got["result"]["content"][0]["text"].as_str().unwrap();
        let value: Value = serde_json::from_str(text).unwrap();
        assert_eq!(value["value"], json!({ "user": 42 }), "{text}");
        let missing = json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "kv_get", "arguments": { "bucket": "sessions", "key": "7" } } });
        assert_eq!(asked(dir.path(), missing)["result"]["isError"], true);
    }

    #[test]
    fn a_bucket_and_a_queue_a_program_opened_from_its_database_are_read_with_that_database() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        let db = store.database("plants").open().unwrap();
        db.bucket::<String>("drafts").open().unwrap().set("fern", &"water weekly".to_owned()).unwrap();
        let queue = db.queue::<Value>("reminders").open().unwrap();
        queue.id("fern-1").delay(std::time::Duration::from_secs(3600)).add(&json!({ "plant": "fern" })).unwrap();
        let called = |id: i64, name: &str, arguments: Value| {
            let message = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": { "name": name, "arguments": arguments } });
            asked(dir.path(), message)["result"].clone()
        };

        let without = called(1, "kv_get", json!({ "bucket": "drafts", "key": "fern" }));
        let why = without["content"][0]["text"].as_str().unwrap();
        assert!(without["isError"] == true && why.contains("read with its database, one of plants"), "{why}");
        let without = called(2, "jobs_list", json!({ "queue": "reminders" }));
        let why = without["content"][0]["text"].as_str().unwrap();
        assert!(why.contains("holds no jobs.db") && why.contains("one of plants"), "{why}");

        let got = called(3, "kv_get", json!({ "bucket": "drafts", "key": "fern", "database": "plants" }));
        let value: Value = serde_json::from_str(got["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(value["value"], "water weekly");
        let listed = called(4, "jobs_list", json!({ "queue": "reminders", "database": "plants" }));
        let jobs: Value = serde_json::from_str(listed["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!((jobs[0]["id"].as_str(), jobs[0]["state"].as_str()), (Some("fern-1"), Some("scheduled")));
    }
}
