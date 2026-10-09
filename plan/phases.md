# Phases

Each phase ends at a gate a check can prove. New modules wait for parity:
nothing in a phase's list is a feature the Go engine did not have, unless it
says so. Mark a task done in the commit that does it.

## 0. Decisions, vocabulary, workspace

- [x] The research read and the owner's decisions written down ([decisions.md](decisions.md)).
- [x] This plan.
- [x] The Go engines leave the tree; `e81a050` stays the reference.
- [ ] AGENTS.md rewritten for the Rust repository: shape, crates, `unsafe`, features, checks.
- [ ] A Cargo workspace with the crates of [architecture.md](architecture.md), building on Linux, macOS and Windows in CI.
- [ ] The API book for jobs, then kv ([dx.md](dx.md#the-api-books)).
- [ ] The protocol's schema in the new vocabulary, and the generator of message types for Rust, TypeScript, Python and Go.

**Gate:** the jobs and kv books pass the newcomer check and the owner accepts
them; the workspace builds and tests on three operating systems.

## 1. Vertical slice: core and KV through every layer

- [ ] The SQLite adapter: the pinned build, the writer and group commit (both shapes measured), readers with idle close, the statement cache, checked migrations, SQLite's memory counted.
- [ ] The runtime: Store, LOCK, clock, memory budget, scheduler, errors.
- [ ] Config and the logger in the core, with the `tracing` layer.
- [ ] kv, complete: buckets, ttl and idle expiry, versions, counters, limiter, quota, once, config storage.
- [ ] The wire: the sans-IO session, dispatch, the server as a sidecar.
- [ ] The pipe: the C ABI, bun:ffi, napi-rs, PyO3, cgo.
- [ ] The Bun, Python and Go SDKs on kv, over the pipe and over a socket.

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
