# TinyStore

An embedded data runtime on SQLite, written in Rust: KV first, then jobs, SQL
databases the application owns, blobs, records and metrics, in one directory
with a file per engine, bounded memory and no daemon. One core serves every
language: Rust programs call the engines directly, and Go, Bun, Node and
Python reach the same core through the wire protocol, embedded in their own
process through a byte pipe, beside it as a sidecar, or remote through the
server. It is a library, so the only thing a consumer sees is a handle and the
promises this file makes about it. `tinyshed/dashbin` is the first caller and
not the owner.

This branch, `rust`, is the rewrite. [plan/](plan/README.md) says what is
decided, what it stands on and the order of work; read it before changing
anything. The Go implementation at commit `e81a050` is the reference: its
tests, [GATES.md](GATES.md) and the design documents in research's
[tinystore/design][design] say what each engine promises and why. Port the
promise, not the code.

This file is the contract for anyone — human or agent — changing the repository.
Keep it short and factual, and update it when a decision moves. Nothing here
should be a fact a ten-second grep would answer.

[design]: https://github.com/tinyshed/research/tree/main/tinystore/design
[design-architecture]: https://github.com/tinyshed/research/blob/main/tinystore/design/architecture.md

## Status

| Part           | What is built                                                                                                                                                                                                                                                                                                                                                         | Contract                                            |
|----------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-----------------------------------------------------|
| runtime        | the store and its `LOCK`, the clock, the memory budget, background work, errors, the engine registry; a store closes at `close` or at its last handle's drop                                                                                                                                                                                                          | [plan/architecture.md](plan/architecture.md)        |
| SQLite adapter | the pinned build, one writer a file with grouped commits that callers lead or that answer through a completion, `query_only` readers that close when idle, checked migrations                                                                                                                                                                                         | [plan/architecture.md](plan/architecture.md)        |
| kv             | buckets of any serde type, branches, ttl and idle expiry, versions, sets, pages, large clears; counters, rate limits, quotas, `once`, transactions                                                                                                                                                                                                                    | [plan/api/kv.md](plan/api/kv.md)                    |
| jobs           | queues of any serde type by their time, ids that add, set, update and cancel, repeats by interval and by cron on a zone, schedules, concurrency in all and by group, rate, retries, steps, workers on the store's threads, claims, pages, watches                                                                                                                     | [plan/api/jobs.md](plan/api/jobs.md)                |
| sql            | databases by name, `sql/<name>.db`, with migrations from files checked at every open and run with foreign keys off; reads that send a write with `returning` to the writer, writes, batches, transactions bounded at five seconds; values through serde; tables and a query builder; buckets and queues kept in its file                                              | [plan/api/sqldb.md](plan/api/sqldb.md)              |
| wire           | frames, the MessagePack profile, a session apart from its transport; protocol 2, its messages written from `protocol/*.wire`, with kv's every method and jobs', a client's worker its jobs and answers on one stream, a watch its job's changes; sql's queries in parts, writes, batches, and transactions whose calls, kv's and jobs' among them, come on one stream | [plan/protocol.md](plan/protocol.md)                |
| pipe and FFI   | a connection in memory to the store in this process, and five C functions over it                                                                                                                                                                                                                                                                                     | [plan/ffi.md](plan/ffi.md)                          |
| server         | `tinystore serve`: stdio for a private child, a Unix socket or a named pipe that `SERVE` names and its proof, TCP and TLS with tokens, `server.stop`, a private server's clock                                                                                                                                                                                        | [docs/wire.md](docs/wire.md#finding-a-local-server) |
| Bun SDK        | kv and jobs as their books have them, and sql's with its tables, query builder and buckets and queues but `include`, over protocol 2: embedded through bun:ffi, through a private child, a sidecar or a remote server; its other engines still speak protocol 1                                                                                                       | [plan/api/kv.md](plan/api/kv.md)                    |

Not built: a transaction of jobs.db, `store.tx` for a queue, sql's `include`, the Python and Go SDKs,
blobs, records, metrics, backup, config and the logger in the core, `tinystore`'s commands but `serve`, the Node, Python and Go
bindings, protocol 2's codecs for Python and Go. [plan/phases.md](plan/phases.md) has
their order and what closes each phase. The guides in [docs/](docs/README.md)
and the SDKs still describe the Go release candidates, and promise nothing for
this branch.

Nothing is released, so nothing is compatible: formats, schemas, the protocol
and every name change freely until `v0.1.0`, the first Rust release. An
engine's schema changes inside its `0001` migration; a second one is added
only after `v0.1.0`.

Do not describe unbuilt behaviour as though it works.

## Shape

| Path                           | What it is                                                                                           |
|--------------------------------|------------------------------------------------------------------------------------------------------|
| `crates/tinystore/`            | the library: the runtime, the SQLite adapter, every engine, the wire, as modules behind features     |
| `crates/tinystore/src/sqlite/` | the SQLite every engine stands on; no engine vocabulary                                              |
| `crates/tinystore/src/kv/`     | kv: buckets, counters, rate limits, quotas, `once`, transactions                                     |
| `crates/tinystore/src/jobs/`   | jobs: queues, ids, repeats and cron, schedules, limits, workers, claims, steps                       |
| `crates/tinystore/src/sql/`    | sql: the application's databases, their migrations, reads, writes, batches and transactions          |
| `crates/tinystore/src/wire/`   | the protocol's bytes and the session that answers them                                               |
| `crates/tinystore/src/pipe.rs` | the connection in memory the FFI carries                                                             |
| `crates/ffi/`                  | the C ABI: `cdylib` for Bun, `staticlib` for cgo                                                     |
| `crates/cli/`                  | `tinystore`: `serve` over stdio, a local socket or a named pipe, TCP and TLS, on tokio               |
| `crates/protocol/`             | the generator: `protocol/*.wire` to the Rust and TypeScript codecs and the vectors; nothing ships it |
| `sdk/js/`, `sdk/python/`       | the clients of the protocol for Bun and Node, and Python                                             |
| `protocol/`                    | the wire protocol's schema, a file an engine, which every codec and vector is written from           |
| `testdata/wire/`               | the protocol's vectors, which Rust and every SDK read                                                |
| `plan/`                        | the rewrite: decisions, evidence, architecture, the API books, phases                                |
| `docs/`, `web/`                | the guides and the docs site, built from `main`                                                      |
| `.agents/skills/`              | how recurring work is done; `.claude/skills/` points to it                                           |

## Crates

One library crate, `tinystore`, so that a Rust program writes `tinystore::kv`
and one crate is published (decision 14). A crate of its own exists only for a
separate artifact: `crates/ffi` and `crates/cli` today, `crates/node` and
`crates/python` later, and `crates/protocol`, a tool that writes code and
ships in nothing. Only `crates/cli` runs tokio; the library runs no async
runtime.

- **Each engine is a cargo feature**, `kv`, `jobs` and `sql` the defaults today. A program links
  the engines it opens and nothing else. An engine adds its calls to `Store`
  from its own module (`impl Store { pub fn bucket… }`): the store's own code
  names no engine.
- **Engines never import each other.** They use the root, `engine` and
  `sqlite`, and nothing of another engine. A file one engine lends another,
  a database's to its buckets and queues, goes through `engine::SharedFile`,
  and `inside.rs` alone names two engines: `db.bucket` and `db.queue`.
- **`unsafe` lives in two places**: `sqlite::memory` and `crates/ffi`. The
  workspace denies `unsafe_code` everywhere else, and clippy's
  `undocumented_unsafe_blocks` makes every block carry a `SAFETY:` comment
  saying what it relies on.
- **A dependency is a decision**, and every program linking TinyStore pays for
  it: rusqlite pinned to one release with SQLite bundled, serde, serde_json,
  tracing. Anything a measurement needs belongs in research, not here.
- **The toolchain is pinned** in `rust-toolchain.toml`; `rust-version` is the
  floor a user needs, raised only for a feature in use.

## Runtime

Breaking one of these is a design change. [architecture.md][design-architecture]
has the reasons as they were designed; [plan/architecture.md](plan/architecture.md)
has them as Rust builds them.

**One directory, a file per engine, unless the application joins two.**
`kv.db`, `jobs.db`, `records.db`, `metrics.db`, `blobs/`, and `sql/<name>.db`
for databases the application names. No engine waits on another's writer,
and no write is atomic across two files. A bucket or a queue opened from an
SQL database, `db.bucket(…)`, lives in that database's file and commits with
its rows: what shares a file shares its writer, by the application's choice
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
the pipe's workers, and the threads a queue's `work` asks for, which its
worker's `stop` and the store's close stop. Engines register periodic work
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
times in arrival order; a query merges segments by time, and a consumer
follows segments in publication order.

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

**A read transaction covers fetching the bytes, not the query.** What one
snapshot hands back stays consistent after it ends, so decoding, merging and
building the answer happen outside it; otherwise a heavy query is itself a
long reader, and the write-ahead log's size is the age of the oldest reader.

**SQL is a named constant beside its function, prepared once a connection.** A
prepared program is not a cached result: each read still starts a new
snapshot. A bound limit is `LIMIT CAST(? AS INTEGER)`, because a bare `LIMIT ?`
lets SQLite expire the program on rebinding and compile it again at `step`.

## The metrics engine's invariants

Built in phase 3; they hold as the Go engine kept them, and breaking one is a
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
figure in `docs/`, `plan/` and research carries what produced it — machine or
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

|                                                                               |                                                                                                                                              |
|-------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------|
| [plan/](plan/README.md)                                                       | the rewrite: what is decided and when, the evidence, the architecture, the API books, the phases                                             |
| [GATES.md](GATES.md)                                                          | every promise and the test that fails when it breaks, by engine; a line names a Rust test once it lands, the Go test at `e81a050` until then |
| [docs/wire.md](docs/wire.md)                                                  | the wire protocol's bytes: frames, credit, MessagePack, errors                                                                               |
| research's [tinystore/design][design]                                         | why each engine has its shape, as designed                                                                                                   |
| [tinyshed/research](https://github.com/tinyshed/research/tree/main/tinystore) | the rounds, every number, the prototypes and the open questions                                                                              |

How recurring work is done is a skill in `.agents/skills/`, which
`.claude/skills/` points to: `api-change`, `sdk`, `wire-change`, `verify`,
`release`, `platform-traps` and `docs`. They still describe the Go
repository where they speak of modules and `task`; fix one where it is wrong
before relying on it, as you would this file.

## Rules that are gates

Every rule worth keeping is worth the twenty lines that make it fail loudly.
[GATES.md](GATES.md) lists each promise beside the test that keeps it; a
change that makes a promise adds its line there.

`just check` runs what CI gates on, so that a failure arrives before a push
instead of after it.

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
  SDK, as [plan/dx.md](plan/dx.md) says: a type is given once, where a handle
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
broken; repairing what the same unpushed work introduced is not that.

**Commit where the work is coherent, not every time it compiles.** A change
that exists only because an earlier one in the same push was wrong belongs
inside that commit, not beside it. A change to what `plan/` says lands in the
commit that makes it true.

Do not commit or push unless you were asked to.

## Branches and releases

The rewrite works on `rust` while `main` builds the docs site; `rust` replaces
`main` when phase 4 ends. Versions are `v0.x.y` until the API stops moving. A
release is one run of the Release workflow, which tags and publishes and never
writes the branch it builds; nothing is tagged by hand. One version covers the
crates, the libraries, the command, npm, PyPI and the Go modules. The format's
and the schemas' versions are not the release's.

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

**What a round proved is worth building is its own commit here**, with its
own type: the evidence and the feature are read by different people.

## Build and checks

```sh
just test        # every crate's tests
just lint        # clippy, a warning failing it
just fmt         # rustfmt
just check       # everything CI gates on
just sdk         # the Bun SDK's kv over the core in process, a private child and a sidecar, and its codecs
just sdk-check   # the Bun SDK's lint and types
just protocol    # every codec and vector written again from protocol/*.wire
just test-linux  # clippy and the tests in a Linux container, for a host that is not Linux
just tables      # every markdown table aligned
```

Install just with `cargo install just`. CI runs clippy and the tests on Linux,
macOS and Windows, each linting the code its platform compiles, and the Bun
pipe on all three.

A timing beside the code is an `#[ignore]` test run by hand with
`--release -- --ignored --nocapture`; its numbers belong in a research round.
Property tests use proptest and decoders get fuzz targets as the codecs land;
a fuzz run is manual.

Run `just check` before pushing. Say which checks you ran and which you could
not.

## Reporting your work

State what changed, which checks ran, and which could not run and why. Point at
exact files and commands. Do not claim a platform was tested from a different
operating system. A number without its environment is not a result. Leave
unrelated working-tree changes alone.
