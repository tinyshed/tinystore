# Your own MCP server

The `mcp` feature of the Rust crate lets your program serve its own tools to
an AI agent over the Model Context Protocol, the way `tinystore mcp` does. A
tool reads by default, and a tool that writes is offered only when your
`--write` flag names it. You can also mount TinyStore's own read tools on your
store next to yours. This is Rust only for now: the Bun, Node, Python and Go
SDKs don't have it.

```sh
cargo add tinystore --features mcp
cargo add serde_json
```

```rust
use serde_json::json;
use tinystore::mcp::{self, Tool};

let store = tinystore::Store::open("data", Default::default())?;
let write = std::env::args().skip_while(|arg| arg != "--write").nth(1); // your --write flag
let name = json!({ "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] });

mcp::server("dashbin", env!("CARGO_PKG_VERSION"))
    .instructions("Read dashbin's dashboards and the store under them.")
    .tools(mcp::read_tools(&store).prefix("store_")) // store_logs, store_kv_get, …
    .tool(Tool::reads("dashboards_list", "The project's dashboards.", json!({ "type": "object" }), |_, _| {
        Ok(json!([{ "name": "latency" }]))
    }))
    .tool(Tool::writes("dashboard_save", "Saves a dashboard.", name.clone(), |args, _| {
        let name = args["name"].as_str().ok_or("dashboard_save takes name")?;
        Ok(json!({ "saved": name }))
    }))
    .tool(Tool::removes("dashboard_delete", "Deletes a dashboard.", name, |_, _| Ok(json!({ "deleted": true }))))
    .allow(write.as_deref()) // None, "all", or "dashboard_save,dashboard_delete"
    .serve()?; // answers stdin until it closes
```

An agent's host starts your program the same way it starts `tinystore mcp`:

```sh
claude mcp add dashbin -- dashbin mcp
claude mcp add dashbin -- dashbin mcp --write dashboard_save
```

## Tools and their marks

A tool is a value: its name, a description for the agent, the JSON Schema of
its arguments, and a closure that answers a call. The constructor you pick is
the tool's mark:

| Constructor     | For a tool that                           | The host sees                                   |
|-----------------|-------------------------------------------|-------------------------------------------------|
| `Tool::reads`   | only reads                                | `readOnlyHint: true`                            |
| `Tool::writes`  | adds or changes something                 | `readOnlyHint: false`, `destructiveHint: false` |
| `Tool::removes` | deletes something or replaces what it had | `readOnlyHint: false`, `destructiveHint: true`  |

The host reads the mark and asks its person before it runs a tool that writes
or removes, as [AI agents](mcp.md#writing) shows. Every tool also says
`openWorldHint: false`.

The closure takes the call's arguments as a `serde_json::Value` and the
call's token, which [Slow calls and cancels](#slow-calls-and-cancels) is
about. It returns `Ok` with JSON, which the agent reads as text. If it returns
an error, the call fails and the agent reads the error's message. So `?` works
on any error type:

```rust
Tool::reads("dashboard_get", "One dashboard.", name, |args, _| {
    let name = args["name"].as_str().ok_or("dashboard_get takes name")?;
    Err(format!("no dashboard {name}").into()) // the agent reads: no dashboard latency
})
```

## The allow list

`allow` takes your `--write` flag's value. A tool that writes or removes is
offered only when the list names it:

| `--write`                         | Offered                          |
|-----------------------------------|----------------------------------|
| not given                         | the tools that read              |
| `dashboard_save`                  | those, and `dashboard_save`      |
| `dashboard_save,dashboard_delete` | those, and both tools that write |
| `all`                             | every tool                       |

- A tool the list doesn't name is not in `tools/list`, and a call of it is
  refused: `dashboard_delete removes, and dashbin mcp was not started with
  --write dashboard_delete`. Its closure doesn't run.
- A name that no tool that writes has is refused before the server answers
  anything. `serve` returns an `Invalid` error with the nearest name:
  `--write dashboard_sav: no tool that writes; did you mean dashboard_save?; all, or dashboard_save, dashboard_delete`.
- The refusals name `--write` and `<name> mcp`, so give your flag that name.
- The server adds one sentence to your instructions. It names the tools that
  change anything, such as `Only dashboard_save changes anything; every other
  tool only reads.`, or says `Nothing here writes.` when the list names none.

> [!WARNING]
> A flag is no boundary for an agent that also has a shell. If the agent can
> run your program's commands itself, `--write` doesn't stop it. Give an
> agent `--write` where its host decides what it may run.

## TinyStore's tools on your store

`mcp::read_tools(&store)` gives the read tools of `tinystore mcp` for the
engines your program is built with: `logs`, `kv_get`, `kv_list`, `jobs_list`,
`sql_query` and `search_list`. They take the same arguments and answer the same
JSON as in [AI agents](mcp.md#tools). `prefix` renames them so that they don't
clash with yours:

```rust
.tools(mcp::read_tools(&store).prefix("store_")) // store_logs, store_kv_get, store_jobs_list, …
```

- Each call opens the store again beside your program, reads, and closes it.
  A call sees everything committed before it, and writes nothing. A
  misspelt bucket or queue name is an error, not a new empty bucket.
- `status` is not among them, because it describes a server and a directory.
  `tinystore mcp` adds it as its own tool.
- A tool that writes to the store is yours to write, with the mark it needs.

## Slow calls and cancels

Each call runs on a thread of its own, so a slow tool doesn't hold up the
others. While one call runs, the server still answers `ping`, `tools/list` and
other calls (contract MCP-05). It writes each answer when its call finishes,
and the host matches answers to calls by their ids. When stdin closes, `serve`
waits for the calls that are still running and writes their answers.

When the agent's host gives up on a call, it sends `notifications/cancelled`.
The server then cancels the call's token and writes no answer for it, as the
MCP spec asks (contract MCP-06). The closure gets that token, a
`tinystore::CancellationToken`, as its second argument:

```rust
Tool::reads("dashboard_check", "Runs each panel's query.", panels, |args, token| {
    let mut checked = Vec::new();
    for panel in args["panels"].as_array().ok_or("dashboard_check takes panels")? {
        if token.is_cancelled() {
            return Err("cancelled".into()); // nobody reads it: the host gave up on the call
        }
        checked.push(run_query(panel)?);
    }
    Ok(json!(checked))
})
```

- Reads through a TinyStore store stop by themselves. The closure runs under
  the token, so a read that is under way when the call is cancelled stops and
  fails with a cancelled error, `ErrorKind::Cancelled` (contract MCP-07). The
  tools of `mcp::read_tools` stop this way too.
- Work outside the store, such as a query on another database, stops only if
  your closure checks `token.is_cancelled()` and stops it.
- A write through the store is not stopped. It runs to its end, even when its
  call is cancelled.
- If a closure panics, its call fails with the panic's message, such as
  `dashboard_check panicked: no panel 7`, and the server goes on.

## Async Rust

If your program runs on tokio or another async runtime, serve your tools
from `tinystore::aio::mcp` with the `async` feature. There a tool's closure
returns a future, so it can await your own async work, such as a query
through `sqlx` or `tokio-postgres`. TinyStore brings no runtime: you await
`serve()` on yours.

```sh
cargo add tinystore --no-default-features --features mcp,async
```

```rust
use serde_json::json;
use tinystore::aio::mcp::{self, Tool};

let pool = sqlx::PgPool::connect(&url).await?;
let sql = json!({ "type": "object", "properties": { "sql": { "type": "string" } }, "required": ["sql"] });

mcp::server("dashbin", env!("CARGO_PKG_VERSION"))
    .tool(Tool::reads("query", "Rows of a read-only statement.", sql, move |args, _| {
        let pool = pool.clone();
        async move {
            let sql = args["sql"].as_str().ok_or("query takes sql")?;
            let rows = sqlx::query(sql).fetch_all(&pool).await?;
            Ok(json!({ "rows": rows.len() }))
        }
    }))
    .serve()
    .await?; // answers stdin until it closes
```

- The closure takes the arguments and the token by value, and returns a
  future that owns what it awaits with. Clone what your calls share, such as
  a pool, into each future.
- Calls run side by side. The future of `serve` polls each call's future on
  the task that awaits it, so the server spawns nothing.
- A blocking tool, a `tinystore::mcp::Tool` or one of `mcp::read_tools(&store)`,
  mounts on the same server. Each of its calls runs on a thread of its own,
  so it doesn't hold up the others.
- When the host cancels a call, the server drops the call's future and
  writes no answer (contract MCP-08). Dropping the future stops whatever it
  awaits. A statement it started on another database stops being awaited,
  but that database may run it until its own timeout.
- Reads through a TinyStore store stop too, even a read that you hand to
  `tokio::spawn`, because the server polls each future under the call's
  token (contract MCP-07).
- With `mcp` and `async` and no engine, your program links no SQLite
  (contract MCP-09).
- `serve` reads stdin on a thread of its own and writes stdout on another.
  If you drop the future of `serve` while stdin is still open, its reading
  thread waits until the next line arrives or stdin closes.

## Limits and defaults

| What                          | Value                                                          |
|-------------------------------|----------------------------------------------------------------|
| Transport                     | stdin and stdout, one JSON-RPC 2.0 message per line            |
| Protocol version              | the client's, or `2025-06-18` when it names none               |
| Methods                       | `initialize`, `ping`, `tools/list`, `tools/call`               |
| Calls that run at once        | no limit: a thread per blocking call, a future per awaited one |
| A store tool's list           | 200 items at most, `logs` 50 by default                        |
| `sql_query`'s rows            | 200 at most: a query that gives more is refused, not cut       |
| Two tools with the same name  | refused by `serve` with an `Invalid` error                     |
| A schema that isn't an object | refused by `serve` with an `Invalid` error                     |

## See also

- [AI agents](mcp.md): `tinystore mcp`, its tools and `--write`.
- [Let an AI agent read the store](../../handbook/production/running-it.md#let-an-ai-agent-read-the-store)
  in the Handbook.
