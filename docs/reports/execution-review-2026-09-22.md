# Execution review: lock profile, defects and runtime proposals

Code inspected: `f2974bc`. This report consolidates one review session for
hand-off to implementers. It complements
[the engine audit](engine-audit-2026-09-22.md) and
[the runtime assessment](../storage-runtime-direction.md). Where they overlap,
it cites the audit section instead of repeating it. Production code was not
changed. Two probes ran: a fresh lock profile through the audit's own observer
script, and an `EXPLAIN` of the ingest UPDATE shapes. Everything else is a code
observation or an explicitly labelled expectation.

## Labels

| label | meaning |
|---|---|
| MEASURED | a number from a named run in this report or in a cited report |
| VERIFIED | a reproducible observation of code, compiled SQL or dependency source; not a timing |
| INFERRED | arithmetic from published numbers; a hypothesis until a run isolates it |
| EXPECTED | a projected effect of a proposal; never quote it as a result |
| PROPOSAL | a design that needs a decision before implementation |

Identifiers: `E` evidence, `C` corrections, `D` defects, `A` proposals,
`X` expected effects, `WP` work packages. Cite them in follow-up work.

## 1. Evidence

### E1. The eight-reader plateau is mostly waiting, and mostly Go-side — MEASURED

Ryzen 7 7700 (8 cores), Docker Desktop on Windows 11, Linux
6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs, 15,910,552 kB MemTotal,
`golang:1.27` (go1.27.1 linux/amd64), modernc SQLite 1.59.0 (SQLite 3.53.4),
libc 1.75.7, engine `f2974bc`. Fixture `s10k` from the performance report:
10,000 series × 500 samples, SHA-256
`4d41358bd6c0854baffba05829406090559237803390bd8e12eac4c2da44c436`. Shape `hour`:
one series (`__name__`, `host` matchers), a randomized one-hour window,
360 samples. Warm, sequential runs on the `tsperf` named volume.

Two captures of the same five-second, eight-reader profile exist: the audit's
(`audit.mutex`, binary build `f0c6c2e4…`, 17:23:46 UTC) and a fresh run of the
audit's observer script for this report. Unprofiled three-second runs from the
fresh capture:

| readers | qps, observer on | qps, observer off |
|---:|---:|---:|
| 1 | 8,285.6 | 8,241.2 |
| 8 | 8,991.5 | 9,246.2 |

During the profiled eight-reader run the CPU profile holds 23.80 s of samples in
5.01 s: **4.75 of 16 CPUs busy** with eight readers.

Share of total sampled lock delay (fresh 13.38 s, audit 13.37 s):

| waiting on | fresh | audit | mechanism |
|---|---:|---:|---|
| Go runtime locks, total | 56.8% | 56.0% | |
| — GC: safe-point functions, forEachP, gcstopm, stop-the-world | 27.8% | — | 478–482 Go allocations per query |
| — scheduler: gosched, findRunnable, wakep, stopm, startm | 27.6% | — | goroutine wake-ups and churn |
| `database/sql` `DB.mu` via `addDep` + `removeDep` | 20.0% | 20.4% | pool-wide mutex, twice per executed statement |
| SQLite page-cache mutex (`pcache1Fetch`, `pcache1Unpin`) | 7.3% | 7.3% | one process-wide mutex, see E3 |
| other SQLite mutexes | 1.8% | 1.8% | |
| `context.cancelCtx` | 6.7% | 7.0% | per-statement cancellation plumbing |
| `codec.(*Codec).Decode` | 2.6% | 2.9% | our shared decoder mutex |
| libc `malloc`/`free` | 0.9% | 1.0% | modernc libc allocator mutex |

The audit reported only the codec row. The split of the runtime row comes from
`pprof -peek '^runtime.unlock$'` on the fresh capture.

Mechanisms, VERIFIED in source:

- Go 1.27.1 `database/sql`: `(*DB).addDep` locks `db.mu` (`sql.go:749-753`);
  every `Rows` created with a cancellable context starts `go rs.awaitDone`
  (`sql.go:3007`); every transaction starts `go tx.awaitDone()` (`sql.go:1930`).
- `modernc.org/sqlite@v1.59.0`: `stmt.go:105` (exec) and `stmt.go:295` (query)
  call `interruptOnDone` whenever `ctx.Done() != nil`: a goroutine, a channel and
  a mutex per executed statement (`sqlite.go:77-117`).
- `metrics/query.go:75` always wraps the caller's context with `WithTimeout`, so
  every statement of every read pays both mechanisms.

INFERRED: about 84% of the observed wait is Go-side — runtime GC and scheduling,
the `database/sql` pool mutex and per-statement cancellation. It scales with
statements and allocations per query. SQLite's own shared mutex is about 9% and
the codec mutex under 3%. Wait time is not CPU time, and this is one scenario;
wide reads and ingestion may rank differently.

### E2. Every existing-series ingest rewrites both partial indexes — VERIFIED

The probe compiles the ingest UPDATE shapes against the `series_state` schema of
migrations 0001–0003 and prints the b-tree operations of each program. Same
output on go1.27.1 windows/amd64 and in the Linux container above:

```text
go1.27.1 linux/amd64, SQLite 3.53.4
1 ingest state (ingest.go:145)   IdxDelete IdxDelete Delete IdxInsert IdxInsert Insert
2 head tail (head.go:222)        Delete Insert
3 merged, index columns unset    Delete Insert
4 merged, index columns set      IdxDelete IdxDelete Delete IdxInsert IdxInsert Insert
```

SQLite chooses index maintenance by the SET list, not by whether a value changed.
`metrics/ingest.go:145` always assigns `ready` and `next_gc_ts`, so each touched
series deletes and re-inserts its `series_due` entry whenever `next_gc_ts` is not
null, and its `series_ready` entry whenever `ready = 1`. The partial-index
predicates are evaluated at run time. Together with `metrics/head.go:222`, the
existing-series path rewrites the row twice and the index entries once per call.
Not timed yet; E5 is the instrument.

### E3. How modernc builds SQLite — VERIFIED

- The generator line of `modernc.org/sqlite@v1.59.0/lib/sqlite_linux_amd64.go`
  lists `-DSQLITE_ENABLE_MEMORY_MANAGEMENT`, `-DSQLITE_DEFAULT_MEMSTATUS=0` and
  `-DSQLITE_THREADSAFE=1`.
- With memory management, pcache1 runs one unified group:
  `_pcache1_g.FseparateCache = 0` and the group mutex is
  `SQLITE_MUTEX_STATIC_LRU` (`lib/sqlite_g_0000000000000003.go:42364-42366`).
  `_pcache1Fetch` takes it when the group has a mutex (`lib/sqlite.go:26942`).
  SQLite mutexes are Go `sync.Mutex` (`lib/mutex.go:89-90`). So every page fetch
  and unpin, by every connection in the process, serializes there — E1's 7.3%.
- modernc 1.59 offers `sqlite.RegisterPageCache` with the reference
  `modernc.org/sqlite/pcache.Pool`: one cache per connection, pages off the Go
  heap. Registration is process-global and must precede the first open in the
  process (`pagecache.go`, `ErrPageCacheTooLate`).
- The libc allocator is process-global: `allocatorMu sync.Mutex`
  (`modernc.org/libc@v1.75.7/libc_musl.go:152`, taken in `mem_musl.go:30` and
  following) on Linux, `allocMu` in `mem.go` on other targets. E1 puts it under 1%.
- `MEMSTATUS=0`: SQLite's malloc takes no status mutex.

### E4. `zombiezen.com/go/sqlite` — VERIFIED from its published module

v1.4.2, 23 May 2025. It requires `modernc.org/sqlite v1.37.1` and
`modernc.org/libc v1.65.7` (we pin 1.59.0 and 1.75.7), plus `crawshaw.io/iox`,
`github.com/chzyer/readline`, `golang.org/x/text` and `github.com/google/go-cmp`.
It has no `database/sql` driver. It offers `Conn.Prep` (a per-connection
statement cache), `Conn.SetInterrupt(done)` (one interrupt channel per
connection), `Stmt.ColumnBytes(col, buf)`, `Conn.OpenBlob`, `sqlitex.Save` and
`sqlitex.ImmediateTransaction`, and `sqlitex.Pool`. Sources:
[pkg.go.dev](https://pkg.go.dev/zombiezen.com/go/sqlite) and its `go.mod`.

Adding it to the root module would fail `TestTheModuleCarriesOnlyTheEngine` and
tie us to a modernc a year older than ours. It is a benchmark proxy and a design
reference for A2, not a dependency.

### E5. The right write and cache instruments already exist — VERIFIED

modernc exposes `sqlite3_db_status` through `sql.Conn.Raw`
(`dbstatus.go:42-55`): `DBStatusCacheWrite` (pages the connection wrote — WAL
frames for the writer, checkpoint copies excluded), `DBStatusCacheSpill`
(mid-transaction spills), `DBStatusCacheHit`, `DBStatusCacheMiss` and
`DBStatusLookasideMissFull`. With a commit counter and the frame counts returned
by `pragma wal_checkpoint`, they replace the harness's WAL file-growth figure
(audit §1). OS RSS: `VmRSS`/`VmHWM` from `/proc/self/status` in the container.

## 2. Corrections to statements made earlier in this session

These were made in conversation, not in a committed report. They are recorded
so that nobody acts on them.

- **C1.** "The shared codec mutex is the main suspect for the reader plateau" is
  refuted by E1 (under 3%). The mutex remains as D8, at low priority.
- **C2.** "About 190 µs per touched series from whole-tail rewrites, from the
  100k-series stage" attributed the wrong work: that stage registers 500 new
  series per call (audit §1).
- **C3.** "835 WAL bytes per sample" is WAL file growth, not write volume
  (audit §1). Likewise `peak_process_mib` is Go `Sys`, not RSS.
- **C4.** "Maintain spends about 3.3 ms of about 5.1 ms per series in fsync" is
  INFERRED from elapsed times only; commits and syncs were never counted. It
  stays a hypothesis behind A5 and X4.

## 3. Defects

### D1. `Maintain` stops at the first failing series — VERIFIED in code, no test yet

`metrics/packing.go:33` (expiry), `:45` (candidate read), `:49` (clearReady) and
`:66` (publish) return from the whole pass on any error other than
`ErrConflict`. `dueSeries` orders by `next_gc_ts,series_id` and by `series_id`
(`packing.go:79`, `:82`). A series whose head fails to decode (`ErrCorrupt`), or
exceeds a reduced `MaxHeadBytes`/`MaxHeadSamples` (`ErrLimit` from `fetchHead`),
is selected first again on every pass. Retention and sealing then stop for every
series; heads grow to `MaxHeadSamples` and ingestion is refused file-wide.
This is distinct from the audit's starvation (§4), which delays work rather
than stopping it.

Fix: classify errors per series. Context and SQLite/IO errors end the pass.
`ErrCorrupt`/`ErrLimit` of one series quarantine it: a nullable
`series_state.failed_at`, excluded from both due indexes (migration 0007),
counted in `Maintenance` and `Stats`, never blocking other series. Decide
whether ingestion into a quarantined series gets a distinct error.

Accept: one corrupted packed head among healthy ready series; the healthy ones
seal and expire across passes, the quarantine count is visible and survives
reopen.

### D2. Maintenance selection starves later series — reproduced by the audit (§4)

Fix: a round-robin cursor (`series_id > ?`, wrapping; in memory is enough) or an
indexed queue age. Also stop the `ready`/`clearReady` churn when lateness keeps
240 points ineligible (audit §4).

### D3. The existing-series ingest path runs seven statements and rewrites two indexes — VERIFIED (E2)

`registry.go:217` (identity lookup), `registry.go:171` via `lookupLabelIDs`
(variable SQL text, `registry.go:113`), `ingest.go:121` (state read),
`head.go:138` (the same row again), `head.go:222` (tail update), `head.go:225`
(legacy delete), `ingest.go:145` (state update assigning `ready`, `next_gc_ts`).

Fix: read the row once; write one UPDATE that assigns tail and state; assign
`ready`/`next_gc_ts` only when the values read show a change; delete legacy head
rows only when the snapshot came from the legacy table; prepare through A1. The
statement count overlaps audit §9; the index churn is new here.

Accept: `EXPLAIN` of the common path shows shape 3 of E2; per-commit
`DBStatusCacheWrite` falls on a steady append workload; frontier,
duplicate-last-wins and legacy-conversion tests stay unchanged and green.

### D4. A healthy reader is discarded on ordinary errors — VERIFIED, reproduced by the audit (§10)

`internal/sqlite/reader.go:89-90` closes the connection and every cached
statement on any callback error, including a budget `ErrLimit`. Discard only when
`ctx.Err() != nil` or the driver reports a bad or unknown state; otherwise
`transact` has rolled back and the connection stays warm.

### D5. Reader slots rotate FIFO and statement eviction ignores use — VERIFIED

Idle slots cycle through a channel (`reader.go:76`, `:80`), which warms every
connection instead of reusing one (audit §10). Eviction is insertion-ordered: a
hit does not refresh (`reader.go:31-40`). Variable-length IN lists compile one
program per length (`registry.go:395`; `registry.go:113` on the writer). Fix:
LIFO idle slots, move-to-back on hit, one SQL text per shape through
`json_each(?)` or fixed padded chunk sizes.

### D6. A narrow range decodes the whole head — VERIFIED

`head.go:89` decodes every chunk and filters afterwards. The CRC over the whole
tail is checked before chunks are parsed (`head.go:77`), so chunks outside
`[from, to)` can be skipped without losing integrity. The count and endpoint
checks move to chunk headers.

### D7. Ordinary values are wrapped in an envelope whose CRC is verified again — VERIFIED (also audit §11)

`values.go:38-46` copies the body into a legacy envelope, computes a CRC, and
`Codec.Decode` verifies that CRC and copies again, after `decodeBlock` already
checked the block checksum. Fix: a codec entry point for an already verified
value stream.

### D8. One decoder mutex serves every reader and the writer — VERIFIED; MEASURED small

`codec/codec.go:175` holds `c.mu` across zstd and huffman decoding; the zstd
decoder has concurrency 1 (`codec.go:77`); `metrics/binary.go:152` has the same
pattern. The writer decodes heads through the same `s.decoder`. E1 measures it
under 3% of read lock wait; wide reads and mixed ingestion are unmeasured. Fix
when the codec is touched: `RLock` for decode, a pooled huff0 scratch, decoder
concurrency tied to `MaxReaders`.

### D9. Clock ids can be reused — VERIFIED, a trap rather than a current bug

`clocks.id` is `integer primary key` without `AUTOINCREMENT`
(`migrations/0002_clocks.sql:2`), and rows are deleted when their references
reach zero. A cross-query metadata cache (audit §11, A9) must be keyed by digest,
never by id.

### D10. Registered series are never removed — documented, reproduced by the audit (§3)

`MaxSeries` (default 100,000) counts lifetime registrations (`registry.go:241`,
`:266`; `metrics/README.md:28`). Container churn reaches it and new series are
refused forever. See A7.

## 4. Proposals

### A1. Writer statement cache — PROPOSAL, low risk

The writer is one connection (`internal/sqlite/file.go:48`). Reuse the reader's
bounded cache on a pinned `*sql.Conn`, with transactions still begun on that
connection. Never route statements through `Tx.StmtContext`. Variable-text SQL
stays uncached until normalized (D5).

### A2. A ladder for statement execution — PROPOSAL, gated by E1-style profiles

- **L1a.** Point lookups run with `context.WithoutCancel(ctx)`; the deadline is
  checked between statements; a cancellable context remains only on statements
  without a small bound (`matchSeries`). Removes both per-statement goroutines and
  the `cancelCtx` contention.
- **L1b.** Hot statements execute through `sql.Conn.Raw` on modernc's own
  `driver.Stmt` (`QueryContext`, `Next`). Removes `DB.mu` (20% of E1), `Rows`
  objects and `convertAssign`. No new module.
- **L2.** A thin internal layer over `modernc.org/sqlite/lib`, a package of the
  module we already require: `open_v2`, `prepare_v3`, bind, step, column, reset,
  `clear_bindings`, finalize, interrupt, changes, `last_insert_rowid`, errmsg,
  `blob_open` — about twenty entry points. Removes `driver.Value` boxing and the
  second blob copy, adds `blob_open` for blobs and one interrupt per transaction.
  Gate: a `bench/` comparison of `database/sql`, L1b and zombiezen (E4) on
  primary-key lookup, small insert, 8 KiB blob read and a KV-style get. Take L2
  only if L1 leaves a large gap.

This revises the runtime assessment's "does not need a direct-driver or unsafe
fast path" on the strength of E1. Update that document once L1 is measured.
Accept for each rung: the same eight-reader profile, the E1 table before and
after, and the one-to-eight reader throughput ratio.

### A3. Page cache — PROPOSAL, measurement only

Options: a host-level opt-in to `sqlite.RegisterPageCache(pcache.New())`, which
the library must never install itself because it is process-global; or readers
with `mmap_size > 0`, whose read-only pages outside the WAL bypass pcache — its
RSS semantics and I/O-error behaviour would have to be accepted explicitly.
Only after A2, because E1 ranks this third.

### A4. A writer actor with group commit — PROPOSAL, the runtime core for KV

- One goroutine owns the writer connection; callers submit work and wait.
- Loop: `BEGIN IMMEDIATE`; per request `SAVEPOINT` → work → `RELEASE` or
  `ROLLBACK TO`; `COMMIT`; wake every waiter. A request returns only after its
  commit, so `synchronous=FULL` durability is unchanged.
- Bounded by requests, bytes and lock hold time, with admission before
  preparation (audit §2).
- A dequeued request waits for the outcome instead of returning `ctx.Err()`
  while it might still commit.
- Checkpoint policy lives here: between batches, when the WAL exceeds N pages,
  `wal_checkpoint(RESTART)` with a bounded wait, keeping automatic checkpoints as
  the backstop the runtime assessment requires.
- Users: metrics `Ingest` and publication, KV Put/CAS/Delete, blob metadata.

### A5. Batched maintenance with savepoints — PROPOSAL

Stage a byte-bounded set of encoded candidates outside the writer (audit §9) and
publish them in one transaction with one `SAVEPOINT` per series, so an
`ErrConflict` — including a late one from `removeSealed` — rolls back only that
series. Expiry and `clearReady` ride the same transaction. Needs A1; composes
with A4.

### A6. One owning process per file — PROPOSAL

Hold an exclusive lock on a sidecar SQLite file for the Store's lifetime
(`locking_mode=EXCLUSIVE` plus one write); the OS releases it on a crash and a
second process gets a clear error. Prerequisite for authoritative in-memory
counters (`series_count`, `next_payload_id`) and identity or metadata caches.

### A7. A lifecycle for series, and later for keys — PROPOSAL

Bounded maintenance removes series with no head and no groups: postings,
`series_state`, the series row, dictionary references (reference counts or a
periodic sweep), and a `series_count` decrement. Decide the kind and frontier
rules for a label set registered again. Invalidate A6/A9 caches. The same shape
serves KV TTL deletion and blob collection.

### A8. A shared maintenance scheduler — PROPOSAL; extract when KV's sweeper exists

A fair cursor (D2), quarantine (D1), byte-bounded batches with savepoints (A5)
and a per-item work callback.

### A9. Cross-query metadata caches — PROPOSAL

Decoded clocks keyed by digest: select the 32-byte digest, fetch the body only on
a miss. Dictionary id → label, valid while labels are never deleted (A7 must
invalidate). Bounded in bytes. Read-side caches keyed by content digest are safe
without A6; write-side caches are not.

### A10. Batched snapshot reads — PROPOSAL (audit §6 agrees)

Per chunk of series: heads through `json_each(?)` joined to `series_state`;
group descriptors the same way, keeping the correlated "first overlapping group"
subquery per series; payloads by id list after the budgets are checked. One SQL
text per shape whatever the chunk length. Budget semantics: the
`case when length(x)<=?` bound uses the remaining budget at statement start, and
`takeBytes` still refuses after at most one over-budget row.

### A11. Incremental head encoding and a sorted-input fast path — PROPOSAL (audit §8 agrees)

Re-encode only from the first chunk the merge touched, copying earlier chunk
bytes. Format unchanged. Optional second step: prepare merged heads from a read
snapshot outside the writer and validate versions inside, as `publish` already
does.

### A12. Append-only head chunks — PROPOSAL behind a decision gate

`head_chunks(series_id, seq, …)`: ingestion inserts only the new samples;
duplicates resolve by `seq` when reading or merging; `Maintain` compacts, and
ingestion compacts inline beyond K chunks. Take it for metrics only if head
encode/decode still dominates the ingest profile after A1, D3 and A11. It is
the natural head for records either way, because rewriting a log tail on every
append is not viable.

### A13. Streaming reads and bounded parallel decode — PROPOSAL

`Read` returns an iterator (`iter.Seq2`) over results decoded lazily after the
snapshot, optionally in parallel per series under a process-wide admission budget
(audit §2). Lowers peak memory and GC; E1 puts GC at about 28% of lock wait.

### A14. A contract for aggregate sums — PROPOSAL

Audit §12's input gives 1 for the sequential IEEE sum and 0 for the sum of block
sums; its exact mathematical sum is 2. Only a correctly rounded exact sum is
independent of block boundaries. Store each block sum as a nonoverlapping
floating-point expansion (Shewchuk): usually one term, with the second encoded by
the existing prediction flags. Merge exactly, round once. Counter transitions
between blocks come from the stored first and last values. NaN and infinities
travel as flags.

### A15. Density and layout candidates — PROPOSAL, measure first

- Identity digest: an `int64` digest with a non-unique index, confirming full
  labels over every candidate. A unique 64-bit digest would permanently refuse a
  colliding legitimate series. The audit's 32-byte binary digest (§13) is the
  simpler first comparison.
- Column order: migration 0003 appended `tail` before `head_start`/`head_end`,
  so reading the bounds of an overflowing tail (`head.go:138`) walks its overflow
  chain. A table rebuild puts `tail` last.
- Writer `cache_size` is 1 MiB (`file.go:40`), and `prepareIngest` orders by label
  text (`ingest.go:114`), not by `series_id`. Measure spills with
  `DBStatusCacheSpill`/`CacheWrite` before choosing a writer-only cache size and
  a series-id write order. Readers keep their small caches.
- Page size 4, 8 and 16 KiB, per-object `dbstat` plus E5 write counters.

### A16. Last value for instant queries — PROPOSAL, small

`last_ts` and `last_value` in `series_state`, assigned in the merged UPDATE of
D3, not indexed. "Current value of these series" then needs no head decode.

## 5. Expected effects — EXPECTED, not results

| id | metric | current, MEASURED | expected | basis | confidence | falsified if |
|---|---|---|---|---|---|---|
| X1 | `hour`, 8 readers | 9,246.2 qps (E1, observer off); 1 reader 8,241.2 | at least 2× the 1-reader rate after A2 L1a+L1b, D4, D5 | about 84% of lock wait is Go-side (E1); 4.75 of 16 CPUs busy | medium | the ratio stays under 1.3 after the removed rows leave the profile |
| X2 | `hour`, 1 reader | 8,241.2 qps, about 121 µs | 10,000–12,000 qps after audit §5 and §7 plus L1a | ranking 16.7% and label JSON about 3% of CPU (audit profile; fresh `canonicalLabels` 3.07%); about ten statements a query | medium-low | latency falls less than 10% |
| X3 | region hour: 2,500 series, 900,000 samples, 1 reader | 10.2 qps, 98.3 ms ([prepared reads](prepared-reads-2026-09-22.md)) | 20–28 qps (35–50 ms) after A10 | about 4 statements × 2,500 series ≈ 10,000 statements ≈ 98 ms ⇒ about 10 µs each; decode about 33 ns/sample (10% of 121 µs for 360 samples) ⇒ about 30 ms | low-medium | statements fall 100× and latency stays above 70 ms |
| X4 | `Maintain`, 64 full heads | 324–332 ms ([performance](performance-2026-09-22.md)) | 100–150 ms after A5 and A1 | 64 commits × 3.3 ms (one-sample Ingest p50) ≈ 211 ms of the pass (C4) | medium | commit count falls to one and the pass stays above 250 ms |
| X5 | append to an existing series | about 150 µs per touched series (INFERRED: (41.6 − 3.3) ms / 256, mixed 1-reader row; audit caveat on the mutated database) | 60–90 µs after A1, D3 and A11 | seven unprepared statements, two index rewrites, whole-head decode and encode per series | low | CPU per touched series falls less than 30% |
| X6 | 16 concurrent one-sample `Ingest` callers | about 300 calls/s (INFERRED: one commit per call, p50 3.2–3.3 ms, single writer) | 3,000–6,000 calls/s with A4 | one commit per batch; bounded by per-call CPU (X5) on one writer | medium | fewer than two calls per commit on average |
| X7 | WAL pages per steady append commit | unmeasured; the old figure is file growth (C3) | fewer after D3 and the A15 write order and cache | index churn (E2) and spills | direction only | `CacheWrite` per commit unchanged |
| X8 | bytes per sample | TSBS 1.001792 ([label dictionary](label-dictionary-2026-09-21.md)); real corpus 1.496698 on the pre-dictionary layout | execution items: unchanged. Identity `int64`: at most about 70 B per series, about −2.8% on the TSBS file; 32-byte binary: about 24 B per series, about −1% | text digest of 44 B stored twice; page slack may absorb part | arithmetic upper bound | re-measure the real corpus before projecting |
| X9 | whole blocks answerable from summaries | not built | share of 240-sample, 10-second blocks wholly inside one bucket: 1 h ≈ 34%, 6 h ≈ 89%, 1 day ≈ 97% | a block spans 39.8 min; share = (bucket − span) / bucket | arithmetic | — |

## 6. Work packages for hand-off

| WP | contents | touches | depends on | acceptance |
|---|---|---|---|---|
| WP1 | instruments: E5 counters per stage, commit and checkpoint frames, OS RSS; split registration, append, sealing and mixed stages; fix the quadratic `countSamples`; rotating-key reads; fresh fixtures per mixed stage (audit §1) | `bench/perf` | — | a baseline report on `f2974bc` with the new counters |
| WP2 | D1, D2, `clearReady` churn | `metrics/packing.go`, `retention.go`, migration 0007, `Maintenance`/`Stats`, README | — | D1 test; audit §4 probe turned into a passing test |
| WP3 | D4, D5, A2 L1a | `internal/sqlite/reader.go`, `metrics/query.go`, `registry.go` (`fillLabels`) | WP1 to measure | X1, X2 with the E1 table |
| WP4 | audit §5 posting cardinality counters, single-matcher skip, audit §7 validation without JSON, batched matcher ids | `metrics/registry.go`, migrations | coordinate with WP3 and WP6 | every posting over 1,024 test; X2 |
| WP5 | A10, D6, D7 | `metrics/query.go`, `head.go`, `values.go`, `codec` entry point | WP3 SQL-shape helpers | X3; snapshot tests unchanged |
| WP6 | A1, D3, A11, sorted-input fast path | `internal/sqlite/file.go`, `metrics/ingest.go`, `head.go`, `registry.go` writer part | WP1 | E2 shape 3 on the common path; X5; `CacheWrite` per commit |
| WP7 | A5 | `metrics/packing.go`, `retention.go` | WP2, WP6 | X4; per-series rollback test |
| WP8 | A4 with checkpoint policy and admission (audit §2) | `internal/sqlite`, metrics write call sites | WP6, WP7 | X6 at 1, 4, 16 and 64 callers; return-after-commit and cancellation tests |
| WP9 | A2 L1b/L2 comparison including the zombiezen proxy; A3; writer `cache_size`; page size | `bench/` only | WP1 | a dated report with a go/no-go per rung |
| WP10 | A7, plus A6 if caches exist by then | `metrics/registry.go`, `retention.go`, migrations | WP2 | audit §3 probe turned into a passing test; re-registration semantics tested |
| WP11 | A14 contract and a spike: exact expansion sums against block boundaries | `docs/design.md`, `spike/` | — | the audit §12 input returns 2 regardless of boundaries |
| WP12 | A15 identity digest and column order, after re-measuring the real corpus | `metrics/` schema, migrations | WP1 | per-object `dbstat` before and after |
| WP13 | decisions: A12 after WP6's profile; A13 after WP8's admission | — | WP6, WP8 | a dated decision record |

Merge hazards: `metrics/registry.go` (WP3, WP4, WP6, WP10), `internal/sqlite`
(WP3, WP6, WP8), `metrics/packing.go` (WP2, WP7). Sequence those branches.

## 7. Guardrails for implementers

- No new module in the root `go.mod`, zombiezen included; no cgo; keep modernc.
- Library code never installs process-global SQLite configuration by default:
  page cache, soft heap limit.
- `synchronous=FULL` stays; automatic checkpointing stays as a backstop.
- One snapshot per read; decoding after the snapshot; budgets checked before
  bytes are fetched.
- No cross-query cache keyed by `clocks.id` (D9). Dictionary and identity caches
  only with A6 and invalidation from A7.
- No unique 64-bit identity digest.
- No aggregate built from float block sums before A14 is decided.
- The harness's WAL and peak-process figures are not write volume or RSS (C3).
- Every gate test in `AGENTS.md` keeps passing; a deliberate behaviour change
  updates its test and states the new contract.
- Every work package ends in a dated report with environment, fixture hashes,
  commands, and before and after in one run.

## 8. What each future engine reuses

| component | metrics | KV | records | blob |
|---|:-:|:-:|:-:|:-:|
| A4 writer actor, group commit, savepoints, checkpoint policy | yes | essential | yes | metadata |
| A2 statement execution ladder | yes | essential: a get is one statement | yes | yes, plus `blob_open` |
| A8 maintenance scheduler: fairness, quarantine, batches | retention, sealing | TTL | retention | collection |
| A7 lifecycle of a registered name | series | keys | streams | objects |
| A6 single owning process | caches | caches | caches | leases |
| label dictionary and postings | yes | — | likely | — |
| A12 append-only head | decision | — | natural shape | — |
| E5 instruments and the WP1 harness | yes | yes | yes | yes |

## Reproduce

E1, fresh capture. `/perf/review-mutex` must not exist; `s10k` comes from the
performance report's populate phase:

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  golang:1.27 sh -c '
    sh /src/docs/reports/engine-audit-2026-09-22-probes/observer.sh \
      /src /perf/review-mutex /perf/s10k
    p=/perf/review-mutex
    go tool pprof -top "$p/perf" "$p/read.mutex"
    go tool pprof -peek "sync.\(\*Mutex\).Unlock$" "$p/perf" "$p/read.mutex"
    go tool pprof -peek "^runtime.unlock$" "$p/perf" "$p/read.mutex"
    go tool pprof -top -cum -focus=Xpthread_mutex_unlock "$p/perf" "$p/read.mutex"'
```

The audit's capture is `audit.mutex` on the same volume; the same `pprof`
commands apply to it.

E2. The probe is stored as text so that it stays out of the module's packages:

```sh
docker run --rm -v <repo>:/src -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  golang:1.27 sh -c '
    mkdir -p /tmp/probe
    cp /src/docs/reports/execution-review-2026-09-22-probes/explain.go.txt /tmp/probe/main.go
    cp /src/go.mod /src/go.sum /tmp/probe/
    cd /tmp/probe && sed -i "s#^module .*#module probe#" go.mod && go run .'
```

E3–E5 cite files in the Go module cache, as `module@version/path:line`.

## Checks

Ran: E1's fresh capture and the `pprof` decomposition of both captures; E2 on
go1.27.1 windows/amd64 and in the Linux container. Not run: `task check` and the
test suite, because no production code changed; this round adds a report and a
probe kept as `.txt` outside the build.

Conversation labels used before this report, for the requester: p.0 → D1,
p.1 → D8, p.2 → A1, p.3 → D3, p.4 → A5, p.5 → A10 and A2, p.6 → D4 and D5,
p.7 → D6, p.8 → D7, p.9 → D2; A → A4, B → A11, C → A9, D → A14 and X9,
E, F, G, H → A15; the three architectural proposals → A4, A2, A12.
