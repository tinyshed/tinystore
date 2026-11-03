//! The server awaited on tokio's runtime, as a program's `main` awaits it.

use std::io::{self, BufReader, PipeWriter, Write};

use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::timeout;

use super::*;
use crate::mcp::fixture::{answering, call, cancelled, request};
use crate::testing::PATIENCE;

/// A server awaited on a task of the test's runtime: each message goes in
/// when the test sends it, and each answer comes out as it is written.
struct Awaited {
    input: Option<PipeWriter>,
    answers: UnboundedReceiver<Value>,
    served: tokio::task::JoinHandle<Result<()>>,
}

impl Awaited {
    fn start(server: Server) -> Awaited {
        let (reader, writer) = io::pipe().unwrap();
        let (answer, answers) = unbounded_channel();
        let output = answering(move |line| {
            answer.send(line).ok();
        });
        let served = tokio::spawn(server.serve_on(BufReader::new(reader), output));
        Awaited { input: Some(writer), answers, served }
    }

    fn send(&mut self, message: &Value) {
        let input = self.input.as_mut().expect("stdin is open");
        input.write_all(format!("{message}\n").as_bytes()).unwrap();
    }

    async fn next(&mut self) -> Value {
        timeout(PATIENCE, self.answers.recv()).await.expect("an answer in time").expect("the server answers")
    }

    /// Closes stdin, and once `serve` has returned, the answers written since
    /// the last one read.
    async fn close(mut self) -> Vec<Value> {
        drop(self.input.take());
        let served = timeout(PATIENCE, &mut self.served).await.expect("serve returned once stdin closed");
        served.unwrap().unwrap();
        let mut rest = Vec::new();
        while let Ok(answer) = self.answers.try_recv() {
            rest.push(answer);
        }
        rest
    }
}

async fn arrived<T>(told: &mut UnboundedReceiver<T>) -> T {
    timeout(PATIENCE, told.recv()).await.expect("it came in time").expect("its sender is alive")
}

/// Says when the future that holds it is dropped.
struct Dropped(UnboundedSender<()>);

impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.send(()).ok();
    }
}

fn object() -> Value {
    json!({ "type": "object" })
}

// contract: MCP-08
#[tokio::test]
async fn an_awaited_server_answers_a_ping_while_a_slow_call_awaits_and_drops_a_cancelled_calls_future() {
    let ((started, mut starts), (dropped, mut drops)) = (unbounded_channel(), unbounded_channel());
    let slow = Tool::reads("slow_read", "Awaits until it is cancelled.", object(), move |_, _| {
        let (started, dropped) = (started.clone(), Dropped(dropped.clone()));
        async move {
            let _dropped = dropped;
            started.send(()).ok();
            std::future::pending().await
        }
    });
    let listed = crate::mcp::Tool::reads("dashboards_list", "The project's dashboards.", object(), |_, _| {
        Ok(json!([{ "name": "latency" }]))
    });
    let mut server = Awaited::start(server("dashbin", "1.4.0").tool(slow).tool(listed));
    server.send(&call(1, "slow_read", json!({})));
    arrived(&mut starts).await;

    server.send(&request(2, "ping", json!({})));
    assert_eq!(server.next().await, json!({ "jsonrpc": "2.0", "id": 2, "result": {} }), "a ping waits for no call");
    server.send(&call(3, "dashboards_list", json!({}))); // blocking, on a thread of its own
    assert_eq!(server.next().await["result"]["content"][0]["text"], r#"[{"name":"latency"}]"#);

    server.send(&cancelled(1));
    arrived(&mut drops).await; // the slow call's future was dropped
    server.send(&request(4, "ping", json!({})));
    assert_eq!(server.next().await["id"], 4);
    assert_eq!(server.close().await, Vec::<Value>::new(), "the cancelled call got no answer");
}

#[tokio::test]
async fn an_awaited_handlers_error_and_panic_are_the_tools_errors_and_the_server_goes_on() {
    let failing = Tool::reads("dashboard_get", "One dashboard.", object(), |given, _| async move {
        let name = given["name"].as_str().ok_or("dashboard_get takes name")?;
        if name == "broken" {
            panic!("no panel 7");
        }
        Err(format!("no dashboard {name}").into())
    });
    let mut server = Awaited::start(server("dashbin", "1.4.0").tool(failing));
    server.send(&call(1, "dashboard_get", json!({ "name": "broken" })));
    let panicked = server.next().await;
    assert_eq!(panicked["result"]["content"][0]["text"], "dashboard_get panicked: no panel 7");
    server.send(&call(2, "dashboard_get", json!({ "name": "latency" })));
    let failed = server.next().await;
    assert_eq!(
        (failed["result"]["isError"].as_bool(), failed["result"]["content"][0]["text"].as_str()),
        (Some(true), Some("no dashboard latency"))
    );
    assert_eq!(server.close().await, Vec::<Value>::new());
}

// contract: MCP-07
#[cfg(feature = "sql")]
#[tokio::test]
async fn a_read_of_the_store_an_awaited_call_made_stops_once_the_call_is_cancelled_wherever_it_is_awaited() {
    use crate::{ErrorKind, Options};

    // counts to a billion, which takes minutes: a read only a cancel ends
    const SLOW: &str = "with recursive c(x) as (select 1 union all select x + 1 from c where x < 1000000000)
        select count(*) from c";
    let dir = tempfile::tempdir().unwrap();
    let store = crate::aio::Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let db = store.database("app").open().unwrap();
    let ((started, mut starts), (ended, mut endings)) = (unbounded_channel(), unbounded_channel());
    let count = Tool::reads("count", "Counts to a billion.", object(), move |_, _| {
        let (db, started, ended) = (db.clone(), started.clone(), ended.clone());
        async move {
            let counting = db.scalar::<i64>(SLOW);
            // awaited by another task, which the call's future dropped does not stop
            tokio::spawn(async move { ended.send(counting.await).ok() });
            started.send(()).ok();
            std::future::pending().await
        }
    });
    let mut server = Awaited::start(server("dashbin", "1.4.0").tool(count).tools(read_tools(&store)));
    server.send(&call(1, "count", json!({})));
    arrived(&mut starts).await;
    server.send(&cancelled(1));
    let error = arrived(&mut endings).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Cancelled, "{error}");

    let query = json!({ "database": "app", "query": "select 1 as one" });
    server.send(&call(2, "sql_query", query)); // a blocking read tool on the awaited server
    assert_eq!(server.next().await["result"]["content"][0]["text"], r#"[{"one":1}]"#);
    assert_eq!(server.close().await, Vec::<Value>::new());
    store.close().await.unwrap();
}
