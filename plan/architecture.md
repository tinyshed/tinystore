# Architecture

## Layers

Hosts on top, SQLite at the bottom. Below the dashed line only wire frames
cross, whichever way a host reaches the core.

```text
Rust program                Bun · Node · Python · Go program
    │                       SDK: one typed API per language, one vocabulary
    │                            │  wire frames, MessagePack
    │              ┌─────────────┼──────────────────┐
    │          byte pipe      sidecar             server
    │          (FFI)          (Unix socket,        (TCP + TLS,
    │                          named pipe, stdio)   tokens)
    │              └─────────────┼──────────────────┘
    │                 wire session (sans-IO) and dispatch
    └─────────────► engines: kv · jobs · sql · blobs · records · metrics · config · logger
                    runtime: Store, LOCK, clock, memory budget, scheduler, backup
                    SQLite adapter: writer and group commit, readers, statements, migrations, memory
                    SQLite 3.53, static · zstd
```

## Principles

- **The protocol is the API boundary.** Every language but Rust sees only
  messages. A new feature is a message in the schema, its dispatch, and its
  call in each SDK; the FFI never changes ([ffi.md](ffi.md)).
- **`unsafe` lives in two crates**, the SQLite adapter and the FFI. Every
  other crate is `#![forbid(unsafe_code)]`, and every `unsafe` block carries a
  SAFETY comment saying what it relies on.
- **A blocking core with asynchronous edges.** Engines expose blocking calls,
  safe from any thread, and start no async runtime. Only the server runs
  tokio. Hosts with an event loop get their answers through the pipe's wake.
- **Everything is bounded and counted.** One memory budget covers Rust's
  buffers, SQLite's allocations and zstd's contexts. Past a bound is a
  resource error, never a smaller answer.
- **The storage invariants of AGENTS.md hold unchanged** (below). They were
  never about Go.
- **A program pays for what it links.** Each engine is a cargo feature, and so
  are FTS5 and R*Tree. Binary size and idle memory per set of engines are
  measured in CI, as `task size` measures them today.

## Crates

One library, `tinystore`, holds the runtime, the SQLite adapter, every engine,
the wire and the server, as modules behind cargo features: a Rust program
writes `tinystore::kv`, and one crate is published. A crate of its own exists
only where a separate artifact does.

| Folder              | Package            | What it holds                                                                  |
|---------------------|--------------------|--------------------------------------------------------------------------------|
| `crates/tinystore/` | `tinystore`        | the library: modules below                                                     |
| `crates/cli/`       | `tinystore-cli`    | the `tinystore` executable: serve, status, logs, mcp, backup, restore, migrate |
| `crates/ffi/`       | `tinystore-ffi`    | the C ABI, as `staticlib` for Go and `cdylib` for Bun · `unsafe`               |
| `crates/node/`      | `tinystore-node`   | napi-rs over the same four functions                                           |
| `crates/python/`    | `tinystore-python` | PyO3 over the same four functions                                              |

| Module in `tinystore`                                        | What it holds                                                                                 |
|--------------------------------------------------------------|-----------------------------------------------------------------------------------------------|
| the root                                                     | Store, LOCK, clock, memory budget, background work, errors                                    |
| `engine`                                                     | what an engine uses of its store; applications never import it                                |
| `sqlite`                                                     | the SQLite build, writer and group commit, readers, statements, migrations, memory · `unsafe` |
| `codec`                                                      | the metrics block codec and its entropy coders, in safe Rust                                  |
| `kv`, `jobs`, `sql`, `blobs`, `records`, `metrics`, `backup` | the engines, each a feature; none imports another, which a test checks                        |
| `wire`                                                       | the message schema, the MessagePack profile, the sans-IO session, dispatch                    |
| `server`                                                     | tokio: sockets, named pipes, stdio, TCP and TLS, SERVE, tokens; a feature                     |

`unsafe_code` is denied for the whole workspace; `sqlite::memory` and the FFI
crates allow it, each block with a SAFETY comment.

The SDKs stay where they are: `sdk/js` and `sdk/python` gain the pipe as a
transport, and the Go SDK takes the root module path,
`github.com/tinyshed/tinystore`, so `go get` does not change.

## The stack

| Need               | Choice                                                | Note                                                                                 |
|--------------------|-------------------------------------------------------|--------------------------------------------------------------------------------------|
| SQLite             | rusqlite 0.40, bundled                                | flags pinned: FTS5, R*Tree, JSON in; no shared page cache; `HAVE_FDATASYNC` on Linux |
| Compression        | zstd 0.14 (libzstd 1.5.7)                             | every decoder sets `window_log_max`                                                  |
| Entropy coders     | our own huff0 and FSE                                 | safe Rust, golden vectors; or a format that needs neither                            |
| MessagePack        | rmp 0.8                                               | under the protocol's own profile, as the Go codec is today                           |
| Directory lock     | `std::fs::File::lock`                                 | stable since Rust 1.89                                                               |
| Server             | tokio 1                                               | only behind the `server` feature                                                     |
| Rust logs, metrics | `tracing`, `metrics`                                  | a layer and a recorder, so a Rust program uses the ecosystem's macros                |
| Tests              | proptest, insta, cargo-fuzz, nextest, loom or shuttle | [tests.md](tests.md)                                                                 |

## The runtime model

**Files.** One directory, a file per engine: `kv.db`, `jobs.db`,
`records.db`, `metrics.db`, `sql/<name>.db`, `blobs/`. A bucket or a queue
opened from an application's database instead of the store, `db.bucket(…)`,
`db.queue(…)`, lives in that database's file, so that a transaction commits a
key or a job with the rows it is about. The directory's `LOCK` lets one store
open it; a recovery tool opens it beside the owner, SQL only.

**The writer.** One write connection a file, and grouped commits: writes that
arrive while a commit runs share the next one, each in a savepoint, and a
failed write rolls back alone. A leader among the callers commits the queue,
as in Go; a write that comes through the pipe carries a completion instead of
a waiting caller, and the thread that queued it leads while such writes keep
arriving at the front. research's
[group-commit-contract.md](https://github.com/tinyshed/research/blob/main/tinystore/design/group-commit-contract.md)
is the contract.

**Waiting.** Whatever in the core waits — a commit, another run of a `once`
key, later a job to claim or a message to deliver — has a form that takes a
completion and returns at once, and the blocking Rust call is that form plus a
wait. The pipe answers through completions only, so a host's event loop never
blocks and no store thread is held by a host: a write is queued with its
completion, a run waiting for another is a callback on its end, a watch is a
callback after a commit. A host gets its answers through `recv`, woken by
`wake`. A transaction is the exception that proves it: across a language
boundary it never holds the writer while the host awaits, so the SDKs commit
what a function read and wrote as one checked batch.

**Readers.** A pool a file, `query_only`, every pragma set on every
connection. One stays open; the others close after a minute unused. Each
keeps its prepared statements in a bounded LRU. A read transaction covers
fetching the bytes; decoding and folding happen after it ends, and it has a
deadline, because the WAL's size is the age of the oldest reader.

**Memory.** A store-wide budget of bytes. Work reserves before it allocates;
SQLite's own allocations are counted through `SQLITE_CONFIG_MALLOC` (set
before SQLite initializes) or `sqlite3_status64`, which phase 1 measures
against each other; page caches are small and per connection.

**Background work.** Only the store starts threads that outlive a call: the
scheduler's timer, the writers if phase 1 chooses them, the pipe's workers,
and a jobs worker's loop and handlers, which its `stop` and the store's close
end. `background: false` stops all periodic work, for tests.

**Time.** `Store::now()` is the one clock, injectable; tests use a clock that
only moves forward, which the server lets an SDK move, as today.

**Errors.** One error type with a kind and what failed: `invalid`,
`not found`, `conflict`, `too old`, `too new`, `limit`, `closed`, `corrupt`,
`in use`, `suspended`, `unavailable`, `cancelled`, `outcome unknown`. Each
kind is one wire code and one error class in every SDK.

**Durability.** A setting of each engine:

| Value    | What a commit does                                                  | Survives                                        | Default for      |
|----------|---------------------------------------------------------------------|-------------------------------------------------|------------------|
| `'full'` | WAL, `synchronous=FULL`: syncs before it returns                    | crash of the application, of the OS, power loss | kv, jobs, sql    |
| `'os'`   | WAL, `synchronous=NORMAL`: written to the OS, synced at checkpoints | crash of the application                        | records, metrics |

It is the store's: `open` gives it for every file, and a database may say
its own, with which the buckets and queues kept in it commit. A store has
one, its first opener's: a pipe that joins it, or a client that finds its
sidecar running, asks for the same or for nothing, and is refused another.
With `'os'` one writer sets 22 to 96 times as many keys as with `'full'` on
the round's two hosts, 64 writers 1.2 to 2.0 times, a thousand as many
([evidence.md](evidence.md)).

Counters opened with `flushEvery: '1s'` are kept in memory and written every
second, losing at most that second to a crash, and a rate limit's times always
are; neither is a file's mode, so each has its own word.

## Config

Config lives in the core and is the same in every language: the host declares
its settings' shape (a struct, a dataclass, an object) and the core does the
rest.

The draft order, waiting for the owner, is one line:

```text
code  →  files  →  changes at run time  →  environment      (the last wins)
```

- The order is fixed and never depends on the order of arguments.
- The environment pins a setting: a setting it gives cannot change at run
  time, and `update` refuses with the variable's name.
- `explain()` says where each value came from: `limits.rps = 300 (APP_LIMITS_RPS)`.
- A conflict is an error, never a silent winner: `X` beside `X_FILE`, a value
  of the wrong type. Every bad variable is reported at once.
- The core parses TOML, YAML and JSON, one parser for every language. Through a
  sidecar, the SDK sends its own environment by prefix, since the environment
  belongs to the application and not to the sidecar.
- Changes at run time are kept in kv and reach every process at once.

## Logger

The records engine, the console format and the redaction of secrets live in
the core, once.

- **Rust:** a `tracing` layer. `tracing::info!(user = 42, "signed in")` reaches
  the console and records; spans give the trace and span. A recorder for the
  `metrics` crate sends `counter!("http.requests")` to the metrics engine.
- **Other languages:** a thin logger in the SDK (an `slog.Handler` in Go, a
  logger in Bun and Python) hands each record to the core. Embedded, the core
  redacts and formats the line and returns it from `send`, and the SDK prints
  it to its own stderr, so it keeps its place among the application's other
  output.
- **Through a sidecar:** a round trip of 40–200 µs a line is too slow, so the
  SDK prints with a small formatter of its own, checked against the same
  console vectors. It is the only duplicate left.
- `LOG_*` and the logger's options come through the config layers.

## Later modules

**Channels (phase 5).** `publish` and `subscribe` on named channels. In
memory, delivered at most once, a bounded queue a subscriber, and an overflow
delivers a gap event rather than silence. A message published inside a
transaction is delivered only after the commit. Engines emit their events
(a key changed, a job finished) from their own code after the commit returns:
SQLite's `update_hook` misses WITHOUT ROWID tables and `commit_hook` runs
before the commit is final. Other processes subscribe through the server.
Durable delivery is a job or a records follow, not a channel.

**Locks (phase 5).** Leases with a time to live, and semaphores, kept in kv's
file. jobs' `concurrency.group` is one of them, so the two share a word and an
implementation.

**Indexes (phase 6).** Declared in sqldb's schema: full text over columns,
geo over coordinates. TinyStore keeps the virtual tables and their triggers,
records each definition's hash in `_tinystore_indexes`, builds a changed one
beside the old in the background and switches when it is ready. Vectors come
later, through sqlite-vec's exact search until its approximate search leaves
alpha.

**Compute is not a module.** Each operation lives beside its data and is built
for a real caller: anomalies are an operator of metrics queries, a diff
compares kv versions, similarity is an SQL function.

## One file, several engines

A database lends its file to the buckets and queues opened from it, so that a
transaction commits a key or a job with the rows it is about, and no engine
imports another for it. Three things at the crate's root carry it:

| What                      | It is                                                                                                       |
|---------------------------|-------------------------------------------------------------------------------------------------------------|
| `engine::SharedFile`      | a file one engine opened that others keep their tables in, and the engines kept there, one of each type     |
| `engine::Home`            | where a handle opens: its engine's own file of the store, or a file another engine lends                    |
| `Transaction`, `TxHandle` | a transaction of one file as the handles it takes in see it: a savepoint a call, its bound, what runs after |

The store's `store.tx` and a database's `db.tx` each hand their `Transaction`
to the handles `tx.with` takes in, which run their calls in it and refuse it
when they are kept in another file. A database's begins on its file before
its function runs; the store's begins on kv.db or jobs.db, whichever its
first handle is kept in, and marks the thread for both from the start, so
that a call around it is refused whichever it turns out to be of. `inside.rs` is the one place two engines meet,
a line a handle: `db.bucket`, `db.queue`.

An engine that lives in another's file:

1. names its tables `_tinystore_<engine>_…` and keeps its migrations under a
   history of its own, which the file's `_tinystore_migrations` checks apart
   from the others' and the application's;
2. keeps its state a file: opened by `Engine::at(&Home)`, the store's own or
   the one in a lent file, and closed without closing a file it did not open;
3. wakes what waits on a commit from `Transaction::after`, never before the
   commit, since a waiter woken early reads nothing and sleeps past it;
4. gives its handles `TxHandle`, so that any transaction of their file takes
   them in;
5. adds its handle to `inside.rs`, `database` to its open message, and its
   calls to the session's `call_in`, which `sql.tx` runs in the transaction.

The engines kept in a lent file open after its owner, so the store closes them
first, while the file is open. records, metrics and blobs keep files of their
own: no engine waits on another's writer unless the application asks it to.
An engine of another crate gets the same seams once they are public, with an
extension trait in place of a line in `inside.rs`.

## Engines of others

An application's own engine is a later feature, but the core is built so it
needs no back door: the built-in engines use only what an outside one would.

- **Built now.** The store keeps one engine of each type, opened by the first
  handle and closed with the store (`engine::Host::engine`), with `attach`,
  `every` and `claim` beside it. kv is built on these and the SQLite adapter
  alone.
- **With the second engine.** A file one engine lends another is
  `engine::SharedFile`, which sqldb showed the shape of. A registry of wire
  methods still waits, where an engine registers its calls and is given its
  byte by name in WELCOME, the built-in ones the same way. Today the session
  matches each engine's byte by hand.
- **The three ways in.** Most applications compose what exists: buckets, SQL
  and queues. Some register SQL functions and virtual tables, the cheapest
  extension. A few write an engine of their own beside ours, on the same file,
  registry and wire.

## What carries over from AGENTS.md

These rules hold unchanged; AGENTS.md has their reasons.

- One directory, a file per engine; no write is atomic across two files.
- Writes known before they run share a commit; a transaction holds the writer
  alone.
- The store opens first and engines open against it; one `close` closes them
  all, the last opened first.
- Engines import no other engine.
- Engine logs never loop back into the store.
- Records: one model for logs and events; a record's order is its event time.
- A time is taken only inside its engine's window, `[now − retention, now +
  skew]`.
- An error names what failed.
- Metrics: a sample is kept bit for bit; the caller is told what it was given;
  a query is bounded before it starts; the sealed frontier is kept; lateness
  follows each series' newest sample; retention clips a read before a summary
  is chosen; a counter's increase is computed while raw samples exist; a query
  sees blocks and head from one snapshot; nothing scans every series.
- A read transaction covers fetching bytes, not the query; readers are
  `query_only`; the writer is one connection.
- A measurement is a number with its environment; a storage claim is a
  division of the file.
