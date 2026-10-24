# TinyStore

An embedded data runtime on SQLite, written in Rust: SQL databases the
application owns, KV, jobs, blobs, search, records and metrics, config, locks
and channels, in one directory with a file per engine, bounded memory and no
daemon. One core serves every language: Rust programs call the engines
directly, and Go, Bun, Node and Python reach the same core through the wire
protocol, embedded in their own process through a byte pipe, beside it as a
sidecar, or remote through the server. It is a library, so the only thing a
consumer sees is a handle and the promises [CONTRACTS.md](CONTRACTS.md)
makes about it; breaking one is a regression whatever the tests say.
`tinyshed/dashbin` is the first caller and not the owner.

What a program can do is in [docs/](docs/README.md): the handbook, a guide an
engine, and the reference generated from the source. The design documents in
research's [tinystore/design][design] say why each engine has its shape.

This file is the contract for anyone — human or agent — changing the repository.
Keep it short and factual, and update it when a decision moves. Nothing here
should be a fact a ten-second grep would answer.

[design]: https://github.com/tinyshed/research/tree/main/tinystore/design
[design-architecture]: https://github.com/tinyshed/research/blob/main/tinystore/design/architecture.md

## Status

Every engine is built in the Rust core, over the wire and in the Bun and
Node, Python and Go SDKs. `v0.1.0`, the first release, was published on
20 October to npm, PyPI, crates.io, GHCR and GitHub, after the candidates
`v0.1.0-rc.6` and `rc.7`, and `v0.1.1` on 24 October. npm and PyPI also hold the Go engine's
`0.1.0-rc.1` to `rc.5`, the tags of the same names stay, and research's
submodule is pinned to `v0.1.0-rc.5`.

| Part           | What it is                                                                                         | Guide                                                |
|----------------|----------------------------------------------------------------------------------------------------|------------------------------------------------------|
| runtime        | the store, its `LOCK`, clock, memory budget, background work, errors and each file's durability    | [inside](docs/handbook/inside/journey-of-a-write.md) |
| SQLite adapter | the pinned build, one writer a file with grouped commits, `query_only` readers, checked migrations | [SQLite](docs/handbook/inside/sqlite.md)             |
| sql            | the application's databases, migrations, tables, a query builder, batches and transactions         | [sql](docs/guides/sql/README.md)                     |
| kv             | buckets, branches, expiry, versions, encrypted values, counters, rate limits, quotas, `once`       | [kv](docs/guides/kv/README.md)                       |
| jobs           | queues, ids, repeats and cron, schedules, concurrency, rate, retries, steps, workers               | [jobs](docs/guides/jobs/README.md)                   |
| blobs          | files by path, uploads, conditions, copies, expiry, checked reads, the scrub                       | [blobs](docs/guides/blobs/README.md)                 |
| search         | indexes by words, by vectors and by both, filters, typos, rebuilds, a table's own index            | [search](docs/guides/search/README.md)               |
| channels       | messages to whoever listens now, a transaction's at its commit                                     | [channels](docs/guides/channels/README.md)           |
| locks          | locks and semaphores with fences, leases over the wire                                             | [locks](docs/guides/locks/README.md)                 |
| config         | typed settings in layers, secrets, live settings kept in kv.db                                     | [config](docs/guides/config/README.md)               |
| records        | streams of events and logs packed into blocks, reads, aggregates, a tail, other programs' lines    | [records](docs/guides/records/README.md)             |
| console        | a logger's lines printed as every SDK prints them, a `tracing` layer without a store               | [console](docs/guides/records/console.md)            |
| metrics        | series, instruments, aggregates, ingest, the store's own metrics, the `metrics` crate's recorder   | [metrics](docs/guides/metrics/README.md)             |
| backup         | one checked zip of every engine while it runs, and its restore                                     | [backup](docs/guides/backup/README.md)               |
| replication    | nodes, a home a file, copies, writes passed home, moves, promotion, a witness                      | [nodes](docs/handbook/nodes/copies.md)               |
| wire           | frames, MessagePack, credit, every engine's calls, written from `protocol/*.wire`                  | [wire](docs/wire.md)                                 |
| pipe and FFI   | a connection in memory to the core in this process, four C functions, PyO3's and napi-rs' modules  | [one core](docs/handbook/inside/one-core.md)         |
| server         | `tinystore`: `serve` over stdio, a socket, a pipe, TCP and TLS, and every command beside it        | [cli](docs/guides/cli/README.md)                     |
| SDKs           | Bun and Node, Python and Go, each through the core in process, a child, a sidecar or a server      | [one directory](docs/handbook/start/directory.md)    |
| async API      | `tinystore::aio`: `aio::Store`, its `tx` and `close`, and every engine's handles, awaited          | [Rust with async](docs/handbook/start/rust-async.md) |
| release        | seven targets, npm, PyPI, crates.io, GHCR, the GitHub release and the Go SDK's tag                 | the `release` skill                                  |

From `v0.1.0` on, a store a release wrote opens in the next: an engine's
schema changes through a migration of its own after `0001`, and the
protocol's messages keep their meaning. Names may still change in a minor
release until `1.0`, and its notes say which.

Do not describe unbuilt behaviour as though it works.

## Traps

What has cost time before, so that it does not again:

- **SQLite's flags live in `.cargo/config.toml`**, `LIBSQLITE3_FLAGS`, and
  reach only builds inside this workspace. A program that builds the crate
  elsewhere, from crates.io or by `cargo install`, sets them itself, as the
  crates' READMEs say; the store logs once when they are missing.
- **Generated files are written, never edited**: the codecs, the vectors
  and `docs/wire.md`'s tables of methods from `protocol/*.wire` (`just
  protocol`), the API pages in `docs/reference/` from the doc comments (`cd
  web && bun run reference`), the README's headline, sample and engines from
  `web/landing.md` (`task readme`). Each has a check that fails when they
  differ.
- **Windows is the slowest platform and the strictest**: files held open,
  paths, pipes, processes and their exit. Read the `platform-traps` skill
  before touching files, processes, stdio or sockets.
- **The Go SDK builds with `GOWORK=off`**; a git-ignored `go.work` may sit
  beside it for an editor. A released Go SDK fetches its release's binary
  and library at the first open; a checkout names them through
  `TINYSTORE_BIN` and `TINYSTORE_LIBRARY`.
- **A release builds from the tag it pushes and never builds again**: what a
  build made wrong is the next version.

## Shape

| Path                                | What it is                                                                                                                                                                                                                                               |
|-------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `crates/tinystore/`                 | the library: the runtime, the SQLite adapter, every engine, the wire, as modules behind features                                                                                                                                                         |
| `crates/tinystore/src/sqlite/`      | the SQLite every engine stands on; no engine vocabulary                                                                                                                                                                                                  |
| `crates/tinystore/src/kv/`          | kv: buckets, counters, rate limits, quotas, `once`, transactions, locks and semaphores                                                                                                                                                                   |
| `crates/tinystore/src/jobs/`        | jobs: queues, ids, repeats and cron, schedules, limits, workers, claims, steps                                                                                                                                                                           |
| `crates/tinystore/src/sql/`         | sql: the application's databases, their migrations, reads, writes, batches and transactions                                                                                                                                                              |
| `crates/tinystore/src/search/`      | search: indexes, their words and postings, the head and its packing, vectors, their codes and clusters, filters, searches, the purge of the gone                                                                                                         |
| `crates/tinystore/src/blobs/`       | blobs: files by path, uploads, checked reads, the scrub, in `blobs/` of the store's directory                                                                                                                                                            |
| `crates/tinystore/src/channels/`    | channels: the logs of the branches listened to, subscriptions, their delivery threads                                                                                                                                                                    |
| `crates/tinystore/src/records/`     | records: streams of events, their rows, reads a page at a time, what a stream keeps                                                                                                                                                                      |
| `crates/tinystore/src/console/`     | a logger's lines printed and its `tracing` layer; no store                                                                                                                                                                                               |
| `crates/tinystore/src/metrics/`     | metrics: series and their labels, heads and blocks, reads, aggregates, instruments, what a name keeps                                                                                                                                                    |
| `crates/tinystore/src/backup/`      | backup: a store's snapshot, the zip and its manifest, the restore                                                                                                                                                                                        |
| `crates/tinystore/src/replication/` | replication: a node's log of each file's commits, its entries and segments, snapshots, links and the writes it passes on                                                                                                                                 |
| `crates/tinystore/src/compress/`    | what records and metrics both pack with: varints, bits read a word at a time, rice codes, zstd, CRC-32                                                                                                                                                   |
| `crates/tinystore/src/wire/`        | the protocol's bytes and the session that answers them                                                                                                                                                                                                   |
| `crates/tinystore/src/aio/`         | the async API: `aio::Store`, `run`'s helpers an engine's awaited calls are built on, and a module an engine                                                                                                                                              |
| `crates/tinystore/src/pipe.rs`      | the connection in memory the FFI carries                                                                                                                                                                                                                 |
| `crates/ffi/`                       | the C ABI: `cdylib` for Bun, `staticlib` for cgo; with `python`, the Python SDK's `tinystore._core`; with `node`, Node's `tinystore.node`                                                                                                                |
| `crates/cli/`                       | `tinystore`: `serve` on tokio, the commands that read a store beside it, backups, nodes and the witness, and their help                                                                                                                                  |
| `fuzz/`                             | the fuzz targets, built on nightly through cargo-fuzz and run by hand                                                                                                                                                                                    |
| `crates/protocol/`                  | the `.wire` language's parser and layout, and what writes every codec and vector; nothing ships it                                                                                                                                                       |
| `sdk/js/`, `sdk/python/`            | the clients of the protocol for Bun and Node, and Python                                                                                                                                                                                                 |
| `sdk/go/`                           | the Go SDK, module `tinyshed.org/tinystore`: a package an engine, since a Go method takes no type parameter, and `internal/` its core, the release it fetches, its codec of the protocol and its console                                                 |
| `protocol/`                         | the wire protocol's schema, a file an engine, which every codec and vector is written from                                                                                                                                                               |
| `editors/wire/`                     | the `.wire` schema's highlighting and snippets: one TextMate grammar for VS Code and RustRover                                                                                                                                                           |
| `testdata/wire/`                    | the protocol's vectors, which Rust and every SDK read                                                                                                                                                                                                    |
| `testdata/console/`                 | a logger's console lines, which Rust and every SDK print byte for byte                                                                                                                                                                                   |
| `docs/`                             | the documentation: the handbook, the guides an engine each, the reference generated from the source, the wire's bytes                                                                                                                                    |
| `web/`                              | the docs site at tinyshed.org/tinystore and its Bun server, which also answers `go get` for the Go module                                                                                                                                                |
| `tools/`                            | scripts the justfile and the workflows run: `quick.ts`, the checks of what changed; `stamp.sh`, a release's version in the workspace; `native.sh`, `release.ts` and `smoke.sh`, a release's build, packing and first try; `Dockerfile`, the server image |
| `.agents/skills/`                   | how recurring work is done; `.claude/skills/` points to it                                                                                                                                                                                               |

## Crates

One library crate, `tinystore`, so that a Rust program writes `tinystore::kv`,
published to crates.io with `tinystore-cli` beside it for `cargo install`. A
crate of its own exists only for a separate artifact: `crates/ffi`, whose
feature `python` builds the Python SDK's extension module from the same pipe
and `node` Node's addon, `crates/cli`, and `crates/protocol`, a tool that
writes code and ships in nothing. Only `crates/cli` runs tokio; the library runs no async
runtime, and its tests of `aio` take tokio as a dev-dependency.

- **Each engine is a cargo feature**, `kv`, `config`, `locks`, `channels`, `jobs`, `sql`, `blobs`, `records`, `metrics` and `search` the defaults today, with `system`, the machine among the store's own metrics, which turns on `metrics`, and `backup`, `replication` and `replication-tls` beside them. `async`, the
  Rust async API `tinystore::aio`, turns on `store`, and each engine's async
  calls where that engine's feature is on; it brings no runtime. A program links
  the engines it opens and nothing else. Each turns on `store`, the store and
  the SQLite under it; `config` turns on kv, whose file keeps its changes,
  and `locks` kv too, whose file keeps their epoch;
  `console`, a logger's lines printed, turns on neither,
  so a program that only logs links no SQLite. `typos`, the words nearest a
  misspelt one, stands on no store; `config` and `search` turn it on, and
  the command line uses it through config. `wire`, the protocol's session
  and the pipe that the server, the FFI and every SDK speak, is a default
  feature that turns on kv, and a Rust program that calls the engines
  directly leaves it out. `just lint` builds the library with each engine,
  the console and the wire alone, `async` alone and beside each engine,
  and every engine with `async` and without the wire. An
  engine adds its calls to `Store` from its own module
  (`impl Store { pub fn bucket… }`): the store's own code names no engine.
- **Engines never import each other.** They use the root, `engine`,
  `sqlite` and `compress`, and nothing of another engine: what two engines
  would both write goes to the root, as `compress` holds the bytes records
  and metrics both pack with. A file one engine lends another,
  a database's to its buckets and queues, goes through `engine::SharedFile`,
  and three files alone name two engines: `inside.rs`, `db.bucket`,
  `db.queue`, `db.search_index` and `queue.adding`, a job of a database's
  batch, whose write goes through `engine::LentWrite`; `log_config.rs`, a store's logger
  reading the config `log`; and `replication/forward.rs`, which runs the
  writes another node passed on through kv's, sql's, search's, jobs' and blobs' handles.
- **`unsafe` lives in five places**: `sqlite::memory`, `sqlite::mutex`,
  `space`, which asks the system for the disk's free space, `crates/ffi`,
  and the command line's `owner_only`, which gives what a Windows server
  keeps to its user, `server\`, `SERVE` and the pipe, a DACL of that user's
  alone. The workspace denies `unsafe_code` everywhere else, each of the
  five allows it at its module's top, and clippy's
  `undocumented_unsafe_blocks` makes every block carry a `SAFETY:` comment
  saying what it relies on.
- **A dependency is a decision**, and every program linking TinyStore pays for
  it: rusqlite pinned to one release with SQLite bundled, serde, serde_json,
  tracing, tracing-subscriber's registry for a logger's layer, jiff for a
  cron's zone and a console line's time, sha2 for blobs' hashes, a key's id,
  a series' identity, a backup's files, a table's row in its index and a log's entries, rustls, with `replication-tls` alone, for a node's links,
  chacha20poly1305 for the values of encrypted
  buckets, zstd for the rows of records, the runs of metrics and a node's links, the
  `metrics` facade, whose recorder the metrics engine is, toml and
  yaml-rust2, without its encodings, for a config's files, fst and
  levenshtein_automata for a misspelt name's nearest words, sysinfo, with `system` alone, for the machine among a store's own metrics,
  unicode-normalization, unicode-segmentation and rust-stemmers, pinned, for
  a search's words, libc on Unix for the disk's free space, and futures-core,
  with `async` alone, for the `Stream` trait every runtime's stream helpers
  take, a trait with no code to run. The command line adds windows-sys on
  Windows alone, the release tokio already builds, for the DACL of a server's
  `SERVE` and pipe: no maintained crate wraps a security descriptor safely,
  and tokio's pipe takes one only through `unsafe`.
  Anything a measurement needs belongs in research, not here.
- **The toolchain is pinned** in `rust-toolchain.toml`; `rust-version` is the
  floor a user needs, raised only for a feature in use.

## Runtime

Breaking one of these is a design change. [architecture.md][design-architecture]
has the reasons as they were designed, and the handbook's
[inside](docs/handbook/inside/journey-of-a-write.md) tells a program's author
how they work.

**One directory, a file per engine, unless the application joins two.**
`kv.db`, `jobs.db`, `records.db`, `metrics.db`, `search.db`, `blobs/`, and `sql/<name>.db`
for databases the application names; a node keeps each file's log in `log/`. No engine waits on another's writer,
and no write is atomic across two files. A bucket or a queue opened from an
SQL database, `db.bucket(…)`, lives in that database's file and commits with
its rows, and a table's index, `db.search_index(…)`, with the rows it
follows: what shares a file shares its writer, by the application's choice
and never by default.

**Writes known before they run share a commit; a transaction does not.** A
grouped write is one savepoint of a commit built before it takes the writer,
so the program's code never runs while a group waits. A transaction holds the
writer alone and pays its own sync, for reads that decide what to write. A
call on a file from inside that file's own transaction, on the same thread,
would wait for itself or miss what the transaction wrote: it fails `Invalid`.

**Everything that waits has a form that takes a completion.** A commit, a
`once` run waiting for another, later a job to claim or a message to deliver:
the core answers each through a callback and blocks no thread of its own, and
the blocking Rust call is that form plus a wait. The pipe answers only through
completions, so a host's event loop never blocks.

**The store opens first, engines open against it.** An engine opens with its
first handle and closes with the store, the last opened first. A store closes
at `close`, or when its last handle is dropped; an engine holds the store
weakly, so that it never keeps it open, and a handle that outlives its store
is `Closed`.

**Only the store starts threads that outlive a call**: the scheduler's thread,
its pool, which the pipe's calls and the async API's run on and whose idle
threads end after a minute, its timer, which ends the patience of the waits
that hold no thread, the locks' timer, the threads channels run handlers on,
and the threads a queue's `work` asks for, which its worker's `stop` and the
store's close stop. Engines register periodic work
with `Host::every`; `background: false` stops all of it, for tests, which run
each engine's maintenance themselves.

**Engine logs do not loop.** Engines log through `tracing` with the target
`tinystore`, a failure of background work once and its recovery once, never a
line a call.

**An error names what failed.** One `Error` with an `ErrorKind` that means the
same in every engine, on the wire and in every SDK, and the item it is about:
`kv bucket sessions: key "42/9f86d0…": conflict`. An engine wraps an inner
error with `within` rather than replacing it.

**A time is taken only inside its engine's window.** An engine that keeps an
observation by its time accepts `[now − its retention, now + skew]`, `now` read
once a call from the store's clock; outside it is `TooOld` or `TooNew` naming
the item.

**Records has one logical model for logs and events, and a record's order is
its event time.** A segment stores its records sorted by event time, equal
times in arrival order, and a block keeps each record's arrival; a merge joins
segments one after another and sorts none, so that a consumer's place still
names its range; a query orders by time and then arrival, and a consumer
follows segments in publication order. What v0.1.0 packed has no arrival and
reads as v0.1.0 read it (NOT-12).

## SQLite

**SQLite is single-writer, and the code says so rather than the reviewer.** A
file has one write connection behind a mutex, so contention is a wait rather
than an intermittent `database is locked`, and its transactions begin
`immediate`, because a deferred one that upgrades on its first write gets a
busy that `busy_timeout` cannot wait out. Readers are `query_only`, so a read
path cannot write by mistake. Every pragma is set on every connection when it
opens, so a reader opened again is the same reader; one reader stays open, and
the others close after a minute unused, since a burst of reads otherwise keeps
their memory for good.

**A statement the store was given changes nothing of its connection.** The
writer is shared by every write of a commit and a reader by whoever reads
next, so SQLite's authorizer refuses a given statement that attaches a file,
begins or ends a transaction or sets a pragma, in a read, a write and a
migration, for every caller; and no connection attaches a file at all. The
authorizer sees a statement once, when it is compiled, and a compiled
statement is found again by its text: the adapter writes its own `commit`
with `own!`, and a given statement that starts as they do is refused.

**SQLite runs on the core's mutexes, a page cache a connection.** Both are
the process's and set before SQLite starts. Its own mutexes park a thread the
moment it finds one held, and every read takes its file's twice: two readers
read less than one. The bundled build's shared cache puts every connection's
pages behind one mutex. A program that started SQLite before the store opened
keeps the mutexes it started with.

**A read transaction covers fetching the bytes, not the query.** What one
snapshot hands back stays consistent after it ends, so decoding, merging and
building the answer happen outside it; otherwise a heavy query is itself a
long reader, and the write-ahead log's size is the age of the oldest reader.

**A checkpoint runs beside the writer, not on its turn.** SQLite's own runs
inside the commit that leaves the WAL 1000 frames long, two syncs every queued
write waits for. A file's writer keeps none: past 1000 frames the next caller
through the file that holds no turn runs one, PASSIVE, on a connection of its
own, and the writer checkpoints itself only at 32,000, which writes that never
pause reach. A file a capture logs and a copy run their own, never these.

**A cancel interrupts its own read and no other.** `sqlite3_interrupt` stops
whatever its connection runs, so a read lends its reader to its call's tokens
only from its first statement to its last, and a token interrupts under the
lock the read takes its reader back with; a reader's progress handler reads
only the tokens of the thread stepping it (`cancel.rs`).

**An engine kept in a file settles before its commits.** Every transaction
of a file runs, before it commits, what an engine kept in it asked for: a
table's index reads the rows whose keys its triggers left. It settles first
what a process beside the store left, then each write's own inside that
write's savepoint, so that a write the index cannot take fails alone, and
last, once, what the writes left together, which no one of them can fail:
kv's revision. A settle runs on the writer, and waits for nothing that may
wait for it.

**A statement's columns are named after its first step**: one kept by its
text and compiled before a migration changed its table is compiled again
there, and its columns before it are the old ones.

**SQL is a named constant beside its function, prepared once a connection.** A
prepared program is not a cached result: each read still starts a new
snapshot. A bound limit is `LIMIT CAST(? AS INTEGER)`, because a bare `LIMIT ?`
lets SQLite expire the program on rebinding and compile it again at `step`.
A table's index writes its statements from the table's names, kept by their
text.

## The metrics engine's invariants

They hold as the Go engine kept them, and breaking one is a
design change.

**TinyStore knows samples, series, labels and time, and nothing of whoever
stores them.** No frame, no dashboard, no HTTP, no product vocabulary — and no
import of a caller, in the code or in the tests.

**A sample is kept bit for bit.** `-0`, NaN payloads and infinities are values
and not edge cases to normalise away. Every transform proves the original bits
come back, one value at a time, or the block goes to another representation.
Whoever draws the chart can round, and nobody can unround.

**The caller is told what it was given.** No silent downsampling, no range
snapped to a coarser grid, no approximate answer dressed as an exact one.

**A query is bounded before it starts, and the bound is not the answer's size.**
Decoded samples, payload bytes, blocks and series are separate budgets, and
exceeding one is a resource error rather than a quietly smaller answer.

**A sealed frontier is kept, and the compactor seals a prefix rather than a
selection.** `sealed_before` is stored per series and only sealing moves it,
inside the transaction that wrote the block. What is packed is what is strictly
before the watermark, not what a `limit` returned. Ingest never moves
`sealed_before`: it refuses what is behind it, keeps the rest, and moves
`max_seen_ts` once at the end of a batch.

**What may be sealed trails the series; silence need not seal anything.** A
counter's increase cannot be added across two blocks overlapping in time, so
lateness is bounded by a watermark that follows the newest sample that series
has shown, and a sample behind it is refused, counted and visible, rather than
reopening a block. There is no forced closure on a wall clock.

**Retention clips the read before a summary is chosen.** A query captures one
`now`, each series its cutoff at it, and reads `[max(from, cutoff), to)`. A
whole-block summary is used only when all its samples belong to that range and
one requested bucket; otherwise raw is filtered first.

**Raw lives as long as its block in the first version**, and a whole block may
be answered from its columns while a block cut by a range is decoded.

**A counter's increase is computed while the raw samples still exist**, on the
samples sorted by time: on `100 → 110 → 5 → 20` the increase is 30, and 30
needs the value it reset from.

**What a query sees is blocks and the head from one snapshot**, since the
compactor writes a block and removes what it packed in one transaction.

**Nothing scans every series.** Postings are in the first version, and finding
due work is an index over one value per series rather than one per sample.

**The format carries its own version, and a released one never moves.**
Codec bodies, group directories and schema histories have their own versions
and golden readers.

**Merging directories must not relocate payloads**: a directory keeps explicit
payload ids, and values and summaries are neither decoded nor recomputed.

## Measurements

**A measurement is a number with its environment, or it is an anecdote.** Every
figure in `docs/` and research carries what produced it — machine or
container, versions, fixture, sample count — and the command that reproduces
it. A candidate is compared against what it replaces on identical input, in
the same run. The number is evidence; the story about why is a hypothesis
until a second measurement separates it from the alternatives.

**A storage measurement is a division of the file, not a total**: a change
that claims to save space says which object it took the bytes from and which
it gave them to, b-tree by b-tree.

**A payload's size is not a file's size.** SQLite stores rows in fixed pages,
so every codec result is reported twice: bytes a sample in the payload, and
bytes a sample in a real file.

## Where things are written down

|                                                                                            |                                                                                                 |
|--------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------|
| [docs/](docs/README.md)                                                                    | what a program can do: the handbook, a guide an engine, the reference generated from the source |
| [CONTRACTS.md](CONTRACTS.md)                                                               | what TinyStore promises a program, by id; a test that keeps one names its id                    |
| [docs/wire.md](docs/wire.md)                                                               | the wire protocol's bytes: frames, credit, MessagePack, errors                                  |
| [protocol/README.md](protocol/README.md)                                                   | the `.wire` language the protocol's schema is written in: its lines, types, methods and layout  |
| research's [tinystore/design][design]                                                      | why each engine has its shape, as designed                                                      |
| research's [tinystore/plan](https://github.com/tinyshed/research/tree/main/tinystore/plan) | the rewrite's plan, books and decisions, kept as history                                        |
| [tinyshed/research](https://github.com/tinyshed/research/tree/main/tinystore)              | the rounds, every number, the prototypes and the open questions                                 |

How recurring work is done is a skill in `.agents/skills/`, which
`.claude/skills/` points to: `api-change`, `sdk`, `wire-change`, `verify`,
`release`, `platform-traps`, `docs` and `delegate`; fix one where it is wrong before
relying on it, as you would this file.

## Rules that are gates

Every rule worth keeping is worth the twenty lines that make it fail loudly.
[CONTRACTS.md](CONTRACTS.md) lists what is promised, never which test keeps it;
a test that keeps a contract names its id in a comment, `// contract: KV-04`,
and `just contracts` fails when a MUST is named by no test.
A change that makes or moves a promise changes that file in the same commit.

CI on Linux, macOS and Windows is the gate, and a red run is real. `just
quick` runs what a change reaches before each commit; `just check` runs all
of CI locally, for a change too wide for `just quick`.

## Editing rules

- **Write for someone who has never seen this code**, reading top to bottom,
  once, without opening another module to find out what a name means. Every
  identifier has to survive that pass. **The fix is never a comment** — rename
  it, delete it, split it, or collapse the two branches into one.
- A comment says what the code cannot: why this way, what broke last time,
  which trap is being avoided. Never restate the line below. A doc comment that
  restates its declaration is worse than none; `missing_docs` stays off for
  that reason.
- **Comment prose** is ordinary prose. A doc comment, and any comment of more
  than one sentence, has a capital and full stops; a short inline label may
  omit the stop.
  - A simple why is **one line**; two when one would be too wide. Wrapped prose
    stays within 80 columns; a single line may run to 100 to keep a small fact
    on one line.
  - A long explanation is paragraphs separated by a blank `//` line, one piece
    of reasoning each, the first sentence the summary.
  - A table, a small diagram or a worked example beats prose when it explains
    the behaviour more directly. A worked example is also a test case, so it
    cannot drift.
  - A field whose meaning its name and type give gets no comment; when a type's
    comment starts explaining fields, move each explanation onto its field.
  - Where durability, concurrency, `unsafe`, SQLite or file-system behaviour,
    protocol state or a memory bound is at stake, say enough that a reviewer
    need not rebuild the invariant from the code.
- **A public API speaks a small, familiar vocabulary**, in Rust and in every
  SDK, as the `api-change` skill says: a type is given once, where a handle
  opens; the daily calls are plain verbs; what the engine chooses goes into the
  builder that opens a handle, never into a call. A proposal shows the call
  site first, and an API slice ends with the newcomer check.
- **A public function reads as a list of steps**: each a call named with a
  verb, the details in the functions it calls. A function over 40 lines needs a
  reason and over 60 is split; no line passes 120 columns; more than four
  parameters become a named value.
- **One byte layout, one parser.** Decoding, reuse and partial reads all start
  from what it returns.
- **No crate- or module-wide `allow`.** An `#[expect]` or `#[allow]` sits on one
  item with its `reason`.
- **Tests sit beside what they test.** A behaviour test of `bucket.rs` is
  `bucket_tests.rs`, included with `#[path]`; a small unit test stays inline in
  `mod tests`. What several test files share is a `#[cfg(test)]` module such as
  `kv/fixture.rs`.
- **A markdown table is aligned**, its cells padded so that the pipes line up as
  an IDE formats one.
- Errors carry what failed and keep their cause; a decoding error names the
  invariant that broke rather than the offset it was standing at.
- Do not weaken or delete a test to make a change pass. When behaviour changes
  on purpose, update the test and state the new contract.
- Do not commit machine-specific paths or a corpus.

## Commits

One Conventional Commits subject line, English, imperative, lower case after
the type, scoped to the area touched.

```
feat(kv): add counters, rate limits and quotas to the rust core
fix(sqlite): stop a reader from writing through a cached statement
ci: test the rust workspace on linux, macos and windows
```

No body, no footers, no trailers of any kind — including co-author and
generated-by trailers. The log is read as a changelog, so the subject carries
the whole message. `feat:` and `fix:` become changelog sections, so pick the
type for how the change reads to a user. A `fix:` says something a user had was
broken; repairing what the same unpushed work introduced is not that. A commit
that renames, removes or changes the meaning of something a program writes
marks its type with `!`, as `feat(config)!: …`: a release's notes list it
first, under Changed names, the notes a minor release owes until `1.0`.

**Commit where the work is coherent, not every time it compiles.** A change
that exists only because an earlier one in the same push was wrong belongs
inside that commit, not beside it. A change to what a guide, a skill or this
file says lands in the commit that makes it true.

Work lands on `main`: commit once `just quick` passes, push, then watch CI.
Ask first before anything that rewrites history, deletes data or costs hours.

**Who writes what.** The owner trusts the Rust code to one model: Opus writes
and edits every line of Rust. Helper agents, Haiku among them, take
inventories, run builds, tests and measurements, and do mechanical chores
outside Rust from an exact brief, as the `delegate` skill writes one; they
never write or edit Rust code.

## Branches and releases

`main` is the only branch, and it builds the docs site. Versions are `v0.x.y`
until the API stops moving. A release is one run of the Release workflow,
which tags and publishes and never writes the branch it builds; nothing is
tagged by hand. The `release` skill has its seven targets, what each package
carries and how it reaches its registry. One version covers the executable,
the libraries, npm, PyPI, crates.io, the image and the Go SDK's tag. The
format's and the schemas' versions are not the release's.

## Research rounds

A round is prototype code and one dated report in
[tinyshed/research](https://github.com/tinyshed/research), under
`tinystore/`, whose own AGENTS.md says how a round is run and written.
Neither is a product, and neither may be quoted as one.

**A round measures a commit of this repository, which is never rewritten.**
Research keeps this repository as a submodule pinned to the commit a round
measured, and a report names that commit, so a rewrite of history that moves
it silently unmakes every measurement standing on it: check what the reports
cite before touching history.

**A corpus is fetched, never committed**, here or in research.

The rewrite's plan, the API books, the decisions and the evidence they stood
on, is research's [tinystore/plan](https://github.com/tinyshed/research/tree/main/tinystore/plan),
kept as it was when the rewrite reached `main`: history, not instructions.

**What a round proved is worth building is its own commit here**, with its
own type: the evidence and the feature are read by different people.

## Build and checks

```sh
just test        # every crate's tests
just lint        # clippy, a warning failing it
just fmt         # rustfmt
just quick       # what changed since the upstream, before each commit
just check       # everything CI gates on, for a change too wide for quick
just sdk         # the Bun SDK's kv over the core in process, a private child and a sidecar, and its codecs
just sdk-check   # the Bun SDK's lint and types
just protocol    # every codec and vector written again from protocol/*.wire
just test-linux  # clippy and the tests in a Linux container, for a host that is not Linux
just tables      # every markdown table aligned
just contracts   # every MUST of CONTRACTS.md named by a test
just fuzz        # a fuzz target of fuzz/ for a minute, by hand, in a Linux container
```

Install just with `cargo install just`. CI runs clippy and the tests on Linux,
macOS and Windows, each linting the code its platform compiles, and every SDK
over the core in a job of its own beside them.

A timing beside the code is an `#[ignore]` test run by hand with
`--release -- --ignored --nocapture`; its numbers belong in a research round.
Property tests use proptest and decoders get fuzz targets as the codecs land;
a fuzz run is manual.

Say which checks you ran and which you could not.

## Reporting your work

State what changed, which checks ran, and which could not run and why. Point at
exact files and commands. Do not claim a platform was tested from a different
operating system. A number without its environment is not a result. Leave
unrelated working-tree changes alone.
