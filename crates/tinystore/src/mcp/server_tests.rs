use std::io::{self, Cursor, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

use super::*;
use crate::ErrorKind;
use crate::mcp::{self, Tool, Tools};

/// What `server` answers to `messages`, a line each.
fn answers(server: Server, messages: &[Value]) -> Vec<Value> {
    let input: String = messages.iter().map(|message| format!("{message}\n")).collect();
    let mut output = Vec::new();
    server.serve_on(Cursor::new(input), &mut output).unwrap();
    let output = String::from_utf8(output).unwrap();
    output.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    request(id, "tools/call", json!({ "name": name, "arguments": arguments }))
}

fn named(listed: &Value) -> Vec<&str> {
    listed["result"]["tools"].as_array().unwrap().iter().map(|tool| tool["name"].as_str().unwrap()).collect()
}

/// dashbin's server: a tool that reads, one that writes and one that
/// removes, each counting its runs.
fn dashbin(runs: &Arc<AtomicUsize>) -> Server {
    let object = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
    let counted = |answer: Value| {
        let runs = Arc::clone(runs);
        move |_: &Value| {
            runs.fetch_add(1, Ordering::SeqCst);
            Ok(answer.clone())
        }
    };
    mcp::server("dashbin", "1.4.0")
        .instructions("Read dashbin's dashboards.")
        .tool(Tool::reads("dashboards_list", "The project's dashboards.", json!({ "type": "object" }), |_| {
            Ok(json!([{ "name": "latency" }]))
        }))
        .tool(Tool::writes("dashboard_save", "Saves a dashboard.", object.clone(), counted(json!({ "saved": true }))))
        .tool(Tool::removes("dashboard_delete", "Deletes a dashboard.", object, counted(json!({ "deleted": true }))))
}

#[test]
fn an_agent_starts_the_server_lists_its_tools_and_calls_one() {
    let runs = Arc::new(AtomicUsize::new(0));
    let read = answers(
        dashbin(&runs),
        &[
            request(1, "initialize", json!({ "protocolVersion": "2025-03-26" })),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            request(2, "ping", json!({})),
            request(3, "tools/list", json!({})),
            call(4, "dashboards_list", json!({})),
            call(5, "dashboard_sav", json!({})),
            request(6, "resources/list", json!({})),
        ],
    );
    assert_eq!(read.len(), 6, "a notification gets no answer: {read:?}");
    let started = &read[0]["result"];
    assert_eq!(
        (started["serverInfo"]["name"].as_str(), started["protocolVersion"].as_str()),
        (Some("dashbin"), Some("2025-03-26"))
    );
    assert_eq!(started["instructions"], "Read dashbin's dashboards. Nothing here writes.");
    assert_eq!(read[1]["result"], json!({}));
    assert_eq!(named(&read[2]), ["dashboards_list"]);
    assert_eq!(read[3]["result"]["content"][0]["text"], r#"[{"name":"latency"}]"#);
    assert_eq!(
        (read[4]["result"]["isError"].as_bool(), read[4]["result"]["content"][0]["text"].as_str()),
        (Some(true), Some("no tool dashboard_sav"))
    );
    assert_eq!(read[5]["error"]["code"], -32601);
}

#[test]
fn a_line_that_is_not_json_is_answered_with_a_parse_error_and_the_server_goes_on() {
    let mut output = Vec::new();
    let input = format!("{{not json\n\n{}\n", request(1, "ping", json!({})));
    mcp::server("dashbin", "1.4.0").serve_on(Cursor::new(input), &mut output).unwrap();
    let read: Vec<Value> =
        String::from_utf8(output).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!((read[0]["id"].clone(), read[0]["error"]["code"].clone()), (Value::Null, json!(-32700)));
    assert_eq!(read[1]["result"], json!({}));
}

// contract: MCP-01
#[test]
fn a_tool_that_writes_is_offered_and_runs_only_when_the_allow_list_names_it() {
    let runs = Arc::new(AtomicUsize::new(0));
    let messages = [
        request(1, "tools/list", json!({})),
        call(2, "dashboard_save", json!({})),
        call(3, "dashboard_delete", json!({})),
    ];

    let read = answers(dashbin(&runs), &messages);
    assert_eq!(named(&read[0]), ["dashboards_list"]);
    let refused = &read[1]["result"];
    assert_eq!(refused["isError"], true);
    assert_eq!(
        refused["content"][0]["text"],
        "dashboard_save writes, and dashbin mcp was not started with --write dashboard_save"
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0, "a tool refused runs nothing");

    let read = answers(dashbin(&runs).allow("dashboard_save"), &messages);
    assert_eq!(named(&read[0]), ["dashboards_list", "dashboard_save"]);
    assert_eq!(read[1]["result"]["content"][0]["text"], r#"{"saved":true}"#);
    assert_eq!(read[2]["result"]["isError"], true);
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    let started =
        answers(dashbin(&runs).allow(Some("all")), &[request(1, "initialize", json!({})), messages[0].clone()]);
    let instructions = started[0]["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.ends_with("Write with dashboard_save, dashboard_delete alone, each a change the program sees."),
        "{instructions}"
    );
    assert_eq!(named(&started[1]), ["dashboards_list", "dashboard_save", "dashboard_delete"]);
}

// contract: MCP-02
#[test]
fn every_tool_carries_its_mark() {
    let runs = Arc::new(AtomicUsize::new(0));
    let read = answers(dashbin(&runs).allow("all"), &[request(1, "tools/list", json!({}))]);
    let marks: Vec<(&str, Value)> = read[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| (tool["name"].as_str().unwrap(), tool["annotations"].clone()))
        .collect();
    assert_eq!(
        marks,
        [
            ("dashboards_list", json!({ "readOnlyHint": true, "openWorldHint": false })),
            ("dashboard_save", json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false })),
            ("dashboard_delete", json!({ "readOnlyHint": false, "destructiveHint": true, "openWorldHint": false })),
        ]
    );
}

// contract: MCP-03
#[test]
fn an_allow_list_that_names_no_tool_that_writes_is_refused_before_anything_is_answered() {
    let runs = Arc::new(AtomicUsize::new(0));
    let refused = |server: Server| {
        let mut output = Vec::new();
        let error =
            server.serve_on(Cursor::new(format!("{}\n", request(1, "ping", json!({})))), &mut output).unwrap_err();
        assert!(output.is_empty(), "nothing is answered");
        assert_eq!(error.kind(), ErrorKind::Invalid);
        error.what().to_owned()
    };
    assert_eq!(
        refused(dashbin(&runs).allow("dashboard_save,dashbord_delete")),
        "--write dashbord_delete: no tool that writes; did you mean dashboard_delete?; all, or dashboard_save, dashboard_delete"
    );
    let read_only = refused(dashbin(&runs).allow("dashboards_list"));
    assert!(read_only.starts_with("--write dashboards_list: no tool that writes"), "{read_only}");
    let none = refused(mcp::server("dashbin", "1.4.0").allow("dashboard_save"));
    assert_eq!(none, "--write dashboard_save: no tool that writes; no tool here writes");

    let twice =
        dashbin(&runs).tool(Tool::reads("dashboards_list", "Again.", json!({ "type": "object" }), |_| Ok(json!([]))));
    assert_eq!(refused(twice), "mcp server dashbin: two tools named dashboards_list");
    let shapeless =
        mcp::server("dashbin", "1.4.0")
            .tool(Tool::reads("dashboards_list", "Its dashboards.", json!({}), |_| Ok(json!([]))));
    assert!(refused(shapeless).starts_with("mcp tool dashboards_list: its input schema is a JSON Schema of an object"));
}

#[test]
fn a_handlers_error_is_the_tools_error_with_its_text() {
    let failing = mcp::server("dashbin", "1.4.0").tool(Tool::reads(
        "dashboard_get",
        "One dashboard.",
        json!({ "type": "object" }),
        |given| {
            let name = given["name"].as_str().ok_or("dashboard_get takes name")?;
            Err(format!("no dashboard {name}").into())
        },
    ));
    let read = answers(
        failing,
        &[call(1, "dashboard_get", json!({})), call(2, "dashboard_get", json!({ "name": "latency" }))],
    );
    assert_eq!(
        (read[0]["result"]["isError"].as_bool(), read[0]["result"]["content"][0]["text"].as_str()),
        (Some(true), Some("dashboard_get takes name"))
    );
    assert_eq!(read[1]["result"]["content"][0]["text"], "no dashboard latency");
}

/// An output whose reader has gone, as a host's pipe once it exits.
struct Gone;

impl Write for Gone {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn the_server_ends_quietly_once_its_host_has_gone() {
    let input = format!("{}\n{}\n", request(1, "ping", json!({})), request(2, "ping", json!({})));
    assert!(mcp::server("dashbin", "1.4.0").serve_on(Cursor::new(input), Gone).is_ok());
}

#[test]
fn a_prefix_names_each_tool_of_a_set_after_it() {
    let tools = Tools(vec![Tool::reads("logs", "Its logs.", json!({ "type": "object" }), |_| Ok(json!([])))]);
    let read =
        answers(mcp::server("dashbin", "1.4.0").tools(tools.prefix("store_")), &[request(1, "tools/list", json!({}))]);
    assert_eq!(named(&read[0]), ["store_logs"]);
}
