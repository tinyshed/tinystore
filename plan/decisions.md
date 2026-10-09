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
call the engines directly, without bytes.

**3. Config and logger move into the core.** They exist three times today, in
Go, TypeScript and Python, held together only by test vectors. Config gets a
real layer system with one fixed order, because today's order depends on the
order of arguments and nobody can tell which source wins. In Rust the logger
is a `tracing` layer and the metrics engine a recorder for the `metrics`
crate ([architecture.md](architecture.md)).

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
Python through PyO3. All three expose the same four functions.

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
artifacts: `crates/cli`, `crates/ffi`, `crates/node`, `crates/python`. What
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

## Open

- The order of config layers: the draft in
  [architecture.md](architecture.md#config) waits for the owner.
- Every name in [dx.md](dx.md) marked draft waits for its engine's API book.
- Whether the Rust API is blocking only or also offers `async` wrappers.
- Crate names on crates.io, the npm scope and the PyPI name, checked free
  before the first release.
