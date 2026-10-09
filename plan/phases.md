# Phases

Each phase ends at a gate a check can prove. New modules wait for parity:
nothing in a phase's list is a feature the Go engine did not have, unless it
says so. Mark a task done in the commit that does it.

## 0. Decisions, vocabulary, workspace

- [x] The research read and the owner's decisions written down ([decisions.md](decisions.md)).
- [x] This plan.
- [x] The Go engines leave the tree; `e81a050` stays the reference.
- [x] AGENTS.md rewritten for the Rust repository: shape, crates, `unsafe`, features, checks; `just` in place of `task`. The skills in `.agents/skills/` still speak of Go.
- [x] A Cargo workspace with the crates of [architecture.md](architecture.md): builds and tests on Windows and in a Linux container (`rust:1.99`).
- [ ] CI building and testing it on Linux, macOS and Windows: written, with the Bun pipe on all three, and green once the branch is pushed.
- [x] The kv book's first draft and one newcomer check ([api/kv.md](api/kv.md)).
- [ ] The jobs book; the kv book's second check; the owner accepts both. The kv book's second check is done; the jobs book is drafted and in its check.
- [ ] The protocol's schema in the new vocabulary, and the generator of message types for Rust, TypeScript, Python and Go.

**Gate:** the jobs and kv books pass the newcomer check and the owner accepts
them; the workspace builds and tests on three operating systems.

## 1. Vertical slice: core and KV through every layer

- [x] The SQLite adapter: the pinned build, one writer with grouped commits that callers lead or that answer submitted writes through a callback, readers with idle close, the statement cache, checked migrations.
- [ ] SQLite's memory in the store's budget through `SQLITE_CONFIG_MALLOC`; today only `sqlite3_memory_used` is read.
- [x] The runtime: Store, LOCK, clock, memory budget, background work, errors, the engine registry.
- [ ] Config and the logger in the core, with the `tracing` layer.
- [x] kv's buckets: any serde type, branches, ttl and idle expiry, versions, sets, pages, large clears, maintenance.
- [x] kv's counters, rate limits, quotas, once, transactions; idle renewals that never wait for a commit.
- [x] The wire's session over frames and the MessagePack profile, every vector of `testdata/wire` passing; kv's open, get, has, set, delete, take, touch and clear.
- [ ] kv's scan, batch, view and the rest of its methods; downloads under credit; CANCEL.
- [ ] The server: sockets, named pipes, stdio, TCP and TLS, SERVE, as `tinystore serve`.
- [x] The pipe: the C ABI and bun:ffi; the Bun SDK opens a store `embedded` and its kv calls pass.
- [ ] napi-rs, PyO3 and cgo over the same five functions; the Python and Go SDKs.

A smoke comparison, not a research round, 9 October on Windows 11 with Bun
1.4.2, FULL durability, medians of five passes: through the existing Bun SDK,
a get took 11.5 µs embedded against 37 µs through the Go sidecar of
`e81a050`, a lone set 1.48 against 1.53 ms, and 2000 sets at once ran at about
63,000 against 42,000 a second. The same 2000 sets as raw frames through
bun:ffi ran at 72,000 to 97,000, and through the pipe from Rust at 138,000.

**Gate:** every kv line of GATES.md has a Rust test and it passes; the SDK
suites for kv pass over the pipe and over a socket on three operating
systems; phase 1's measurements in [ffi.md](ffi.md#what-phase-1-measures) are
taken against Go `e81a050` and written up as a research round.

## 2. sqldb, jobs, blobs

- [ ] sqldb: tables from types, checked migrations, typed reads, `tx` in the protocol, FTS5 and R*Tree features.
- [ ] jobs in the new vocabulary: queues, ids, repeats, `concurrency`, `rate`, steps, watching.
- [ ] blobs: objects inline or a file each, checked reads, scrub.
- [ ] kv and jobs inside an sql database.

**Gate:** their GATES.md lines pass in Rust and through every SDK; Dashbin can
start on sqldb, kv and jobs.

## 3. records and metrics

- [ ] The codecs: huff0 and FSE in safe Rust or a format without them, the word-based reader, the exact accumulator.
- [ ] records: the head, segments, paged reads, follow, retention, redaction, `durability: 'os'` by default.
- [ ] metrics: series, instruments, sealing, exact aggregates, retention, the step rule; the `metrics` crate recorder.
- [ ] Formats chosen by measurement: rowid groups for metrics, records' column layout.

**Gate:** their GATES.md lines pass; records' exact filter, single trace and
absent-context reads are no slower than Go's; every decoder has a fuzz target
that ran clean.

## 4. Backup, the command line, the release

- [ ] Backup and restore; a recovery tool's `openBeside`.
- [ ] `tinystore` serve, status, logs, mcp, backup, restore, migrate, schema.
- [ ] Self-metrics.
- [ ] The thin Go SDK on cgo and a sidecar, under the root module path.
- [ ] Actions builds the libraries, executables, npm packages, wheels and Go modules for seven targets.
- [ ] The guides in docs/ rewritten from the API books; `v0.1.0`.

**Gate:** the release workflow builds and smoke-tests every artifact on every
target.

## 5. Channels and locks

- [ ] Channels: publish and subscribe, after-commit delivery, gap events, through the server.
- [ ] Leases and semaphores on kv; jobs' `perGroup` on them.

**Gate:** a message published in a transaction that rolls back is never
delivered; a subscriber that falls behind receives a gap event.

## 6. Indexes

- [ ] Full text and geo declared in sqldb's schema, rebuilt beside the old
  index and switched when ready.
- [ ] Vectors, and compute operations, for real callers.

**Gate:** a changed index definition rebuilds while reads keep answering from
the old one.
