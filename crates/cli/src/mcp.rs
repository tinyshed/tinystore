//! `tinystore mcp`: the library's MCP server on stdin and stdout, with
//! `status`, the store's tools that read it beside the program or server
//! that holds it, and the tools that write that `--write` names. Each call
//! opens the store again, so it sees what was committed since the last.
//!
//! ```text
//! claude mcp add tinystore -- tinystore mcp ./data
//! claude mcp add tinystore -- tinystore mcp ./data --write kv_set,jobs_add
//! ```

use std::path::Path;
use std::process::ExitCode;

use serde_json::json;
use tinystore::ErrorKind;
use tinystore::mcp::{self, Server, Tool};

use crate::asked::{self, Asked, Done, failed, wrong};
use crate::help;

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run(&help::MCP, args, serve)
}

fn serve(asked: Asked) -> Done {
    let (dir, rest) = asked.dir_or_here(0)?;
    if !rest.is_empty() {
        return Err(wrong(format!("one directory, not {} too", rest.join(" "))));
    }
    let dir = std::path::absolute(&dir).unwrap_or(dir);
    match server(&dir, asked.value("--write")).serve() {
        Ok(()) => Ok(()),
        // an allow list that names no tool that writes: the flag is wrong
        Err(error) if error.kind() == ErrorKind::Invalid => Err(wrong(error.what())),
        Err(error) => Err(failed(error)),
    }
}

/// The server of the store in `dir`: what it holds, the tools that read it,
/// and those that write which `write` names.
fn server(dir: &Path, write: Option<&str>) -> Server {
    let reads = "its status, logs, kv values, jobs, SQL databases and search indexes";
    mcp::server("tinystore", env!("CARGO_PKG_VERSION"))
        .instructions(&format!("Read the TinyStore store in {}: {reads}.", dir.display()))
        .tool(status(dir))
        .tools(mcp::read_tools_in(dir))
        .tools(crate::mcp_writes::tools(dir))
        .allow(write)
}

/// What the store holds, read from its directory and its server, which no
/// tool of the library reads: a program asks its own store.
fn status(dir: &Path) -> Tool {
    let dir = dir.to_owned();
    let does = "What the store holds: its files and their sizes, the server that serves it, and how its node stands.";
    let takes = json!({ "type": "object", "properties": {}, "required": [] });
    Tool::reads(
        "status",
        does,
        takes,
        move |_| Ok(crate::status::json_of(&dir).map_err(|refused| refused.to_string())?),
    )
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use serde_json::Value;
    use tinystore::{Options, Store};

    use super::*;

    /// What `tinystore mcp` on `dir` answers to `message`, if anything.
    fn answered(dir: &Path, message: &Value) -> Option<Value> {
        let mut output = Vec::new();
        server(dir, None).serve_on(Cursor::new(format!("{message}\n")), &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        output.lines().next().map(|line| serde_json::from_str(line).unwrap())
    }

    fn asked(dir: &Path, message: Value) -> Value {
        answered(dir, &message).unwrap()
    }

    #[test]
    fn an_agent_lists_the_tools_and_reads_a_value_beside_the_program_that_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        store.bucket::<serde_json::Value>("sessions").open().unwrap().set("42", &json!({ "user": 42 })).unwrap();
        let started = asked(dir.path(), json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }));
        assert_eq!(started["result"]["serverInfo"]["name"], "tinystore");
        assert!(answered(dir.path(), &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none());
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
