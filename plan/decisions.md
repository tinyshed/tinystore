# Decisions

What the owner decided, with the date and the reason. A later decision that
replaces an earlier one says so in the earlier one's entry; nothing here is
deleted.

## 9 October 2026

**1. TinyStore is rewritten in Rust.** The research of 7–9 October measured
native SQLite and Rust paths at 1.7–6.4× the Go engine on reads and CPU-bound
work ([evidence.md](evidence.md)). Speed is the second reason. The first is one
core for every language: today Bun and Python reach TinyStore only through a
sidecar, at 40–200 µs a call, and their logger and config are written again
in each SDK.

**2. The wire protocol is the API boundary for every language but Rust.** The
owner proposed a dumb FFI of a few universal functions passing bytes, so that
everything new goes into the wire layer and nothing into the FFI. Refined the
same day: the FFI is an in-memory connection that carries whole wire frames,
five functions in C and four in each binding ([ffi.md](ffi.md)). Rust programs
call the engines directly, without bytes. On 10 October the fifth went: the
host gives the bytes the core writes its frames into, so no memory of the
core's crosses and nothing is left to free.

**3. Config and logger move into the core.** They exist three times today, in
Go, TypeScript and Python, held together only by test vectors. Config gets a
real layer system with one fixed order, because today's order depends on the
order of arguments and nobody can tell which source wins. In Rust the logger
is a `tracing` layer and the metrics engine a recorder for the `metrics`
crate ([architecture.md](architecture.md)). On 13 October the owner chose
another way out of that: a config is declared in layers, each declaring
again what the ones before it gave, in the order the program writes them,
which `explain` shows setting by setting; only a setting marked `live`
changes while the program runs, and its change goes over every layer
([api/config.md](api/config.md)).

**4. Go embeds the core through cgo.** cgo is the base. The project takes the
whole build burden: Actions builds a static library for every platform and the
Go modules ship them ready, so an application only needs a C toolchain when it
builds with cgo. The sidecar is Go's second mode, through the same SDK. The
AGENTS.md rule "no cgo, ever" belonged to the Go engine and ends with it.

**5. Everything happens in this repository.** The Go engines leave the tree;
Go comes back as a thin SDK like Bun's and Python's, under the same module
path. Work happens on the branch `rust` while `main` still builds the docs
site.

**6. `v0.1.0` is the first Rust release.** The Go release candidates promise
nothing. Dashbin has not started its move to TinyStore and starts on the Rust
core.

**7. No compatibility with the Go formats.** Nothing was released, so files,
schemas and the protocol change freely, for speed and size first. This
replaces the plan's first proposal, which kept the Go formats to test the two
implementations against each other's files.

**8. Durability favours speed in the hot engines.** records and metrics
default to `durability: 'os'`: WAL with `synchronous=NORMAL`, no fsync at a
commit, which survives a crash of the application and may lose the last
commits on a power loss. kv, jobs and sql default to `'full'`. Every engine can
be switched.

**9. Signals are called channels**, with `publish` and `subscribe` as in
Redis. "Signals" means reactive state in JavaScript (Preact, Solid, the TC39
proposal), and "events" is already what records stores.

**10. Bun loads the core through bun:ffi,** although Bun calls it
experimental; the owner wants to try it. Node loads it through napi-rs and
Python through PyO3. All three expose the same four functions. On 18
October the owner had PyO3 live in `crates/ffi`, behind its feature
`python`, rather than in a crate of its own: one crate is every other
language's way into the core's process. PyO3 stays over ctypes because the
research round `ffi-window` measured PyO3 alone barely faster (500k against
490k keys a second) and frames read and written in Rust 1.5 times faster,
which only an extension module can do.

**11. jobs' limits nest.** `concurrency: { total: 8, perGroup: 2 }`, with
`concurrency: 8` as the short form of `{ total: 8 }`. `rate` stays beside it:
it answers how often a job may start, not how many may run. Later the same
day the owner shortened `perGroup` to `group`, since inside `concurrency` it
can only be a limit: `concurrency: { total: 8, group: 2 }`.

**12. New modules come after parity.** Channels and locks in phase 5,
indexes in phase 6. Compute is not a module: each operation lives beside its
data and is built for a real caller ([architecture.md](architecture.md)).

**13. Only Opus writes Rust.** Helper agents gather facts, run checks and do
chores; they never write or edit Rust code.

**14. One library crate, not a crate a layer.** The owner found
`tinystore-core`, `tinystore-sqlite` and the rest odd to read. The runtime, the
SQLite adapter, the engines, the wire and the server are modules of one crate,
`tinystore`, behind features; crates of their own exist only for separate
artifacts: `crates/cli` and `crates/ffi`, whose features build Python's module and later Node's. What
separate crates enforced is kept by lints and a test: `unsafe` is denied
everywhere but `sqlite::memory` and the FFI, and no engine module imports
another.

**15. Keys and jobs that commit with rows are opened from the database.**
The owner found `{ in: db }` an awkward way to say where a bucket lives. A
bucket or a queue opened from the store lives in the store's own file, kv.db
or jobs.db, with a writer of its own; one opened from an application's
database, `db.bucket('sessions')`, lives in that database's file, shares its
writer by the application's choice, and commits with its rows in `db.tx`. The
handle is the same type either way. Whether it reads `db.bucket` or
`db.kv.bucket` waits for the kv book.

**16. The jobs book is accepted** in its second version
([api/jobs.md](api/jobs.md)): a handler gets the value and then the run, and
returns its answer; `add` writes only a free id and `set` makes the id's job
what it says; one word, `concurrency`, bounds the store and a worker; a
schedule is given with its handler. The owner kept three choices the
reviewers or the survey questioned: `dedupe` rather than
`idempotencyWindow`, `step` rather than `checkpoint`, and a cron that needs a
`timeZone` where most libraries assume UTC, since a required zone can be
relaxed later without breaking a caller and a default cannot be tightened.

**17. The sqldb book is accepted** ([api/sqldb.md](api/sqldb.md)), after a
newcomer check and two reviews. SQL stays SQL, and a query builder in every
SDK writes the SQL a person would, without its own wire calls; `batch` is a
list of statements in a shared commit and `tx` a function that holds the
writer five seconds at most and runs once; `all`, `one` and `scalar` take a
write with `returning`, routed by what SQLite says of the statement; `include`
waits for a prototype. Whether kv's `store.tx` over the wire stops running its
function again is a decision of its own.

**18. The records book is accepted** ([api/records.md](api/records.md)),
after two newcomer checks, a blind comparison and the owner's review. A
program meets the engine as `store.logger` and `store.events`: one word for
what is written and what is read, and `store.events()` without a name reads
every stream. `keep` is a stream's own, kept in its file; `tail` gives events
live and `poll` every event once from a cursor; `count`, `top` and
`aggregate`, `streams` and `fieldNames` carry Dashbin's B5 and B6; `search`
reads the values of fields too. The storage is the Go engine's, measured
there, and changes only by a measurement; zstd comes with the feature.

Against entry 3, an SDK's logger prints its own lines however its store is
reached, and the core prints a Rust program's and the command's. A sidecar's
round trip a line is too slow, so an SDK has its formatter either way, and a
second path through the core for a store in the process would be one more to
differ. The console's vectors keep a line the same bytes in every language,
and a secret is hidden before its event leaves the program.

**19. The metrics book is accepted** ([api/metrics.md](api/metrics.md)),
on 11 October, after a newcomer check, a blind comparison and four readings
by the owner's reviewer. A program meets the engine as `store.metrics`:
counters, gauges and timers it keeps, `ingest` for samples from elsewhere,
and `read`, `latest` and `aggregate`, whose `every` and `by` are records'.
The Go engine's eighteen options become a name's own `keep` and `buckets`,
and the engine's choices. Instruments send what changed and the store keeps
each total, so that processes sharing a store count into one series; each
run of a total starts with a zero written at its start, so that a reset is
seen whatever the new count. `lookback` is gone: the first bucket steps from
the newest sample before the range, and a bucket after a gap says so.
Dashbin's B2 is a timer's `buckets`, whose percentile is an interval, `lower`
to `upper`, beside a gauge's exact percentile by rank; B6 is `names`,
`labelNames` and `labelValues`. Rollups, Prometheus' text and `poll` (C4, C5,
C6) are shapes for later.

**20. Every store is a node, and a file has one home** ([scaling.md](scaling.md)),
on 17 October, after search closed. The owner wants replication and scaling
built now and the command line and SDKs last, once the system's bounds are
known. Every TinyStore is a node running the same code: it takes the files it
asks for `from` other nodes and `serve`s whatever it holds, so a relay, a
standby or a replica is only who takes what from whom. A file is written by
one node at a time, its home, since two versions of one page cannot be merged;
different files may have homes on different nodes. A write goes to its file's
home and answers once the node's own copy holds its commit; a read is local;
operations that commute, a counter's add and an append, go through a local
queue and are seen at once. Migrations run at the home alone. In the first
version a person moves a home with `promote`; a judge outside the nodes comes
later. An outside review the owner brought held the three jobs apart: copying,
routing through the catalog, and the right to write, which the catalog never
grants. The network is the core's, so that a store inside a program is the
same node as a server; a node's logs and metrics stay its own.

## Open

- Every name in [dx.md](dx.md) marked draft waits for its engine's API book.
- Whether the Rust API is blocking only or also offers `async` wrappers.
- Crate names on crates.io, the npm scope and the PyPI name, checked free
  before the first release.
- Replication: the calls under [scaling.md](scaling.md)'s Open; those the
  owner took on 18 October are under its Decided.
