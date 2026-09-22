# First durable metrics slice

Historical first-slice report. [The packed-head update](packed-head-2026-09-21.md)
supersedes its row-head description and TSBS size; the earlier measurements
below remain as the comparison baseline.

Implemented in `metrics/`, backed by the new domain-neutral `internal/sqlite/`.
Neither package imports `spike/`. Records, KV and SQL mapping remain independent
future consumers, with no placeholder packages or universal object manager.

## Reading the code in order

Start with [the executable example](../metrics/example_test.go), then follow:

```text
Open       open.go → internal/sqlite → embedded SQL migrations
Ingest     ingest.go → registry.go → exact durable head
Read       query.go → one snapshot → owned bytes → decode outside snapshot
Maintain   retention.go → packing.go → version check → atomic publication
Close      stop admission → drain active calls → close codecs and pools
```

Binary work is separate: `groups.go` owns addressing and the version-one
reader, `directory.go` the compact version-two directory, `clocks.go` timestamp
coding and ownership, `values.go` the representation selector, `residuals.go`
grid corrections, and `binary.go` bounded parsing/compression. Their helpers
name operations so ingest/query/retention need not manipulate bit fields.

## Implemented behavior

- Exact series identity, postings for equality matching, cardinality and label
  limits, immutable gauge/counter kind.
- Atomic bounded ingestion, replacement of mutable duplicates, explicit
  rejection behind retention/frontier, rejected-batch statistics.
- Exact raw range reads with separate byte/sample/series/block budgets and a
  snapshot deadline. Registry, head, directories, clocks and payloads are read
  in the same snapshot; returned slices retain no SQLite reader.
- A durable row head; bounded safe-prefix sealing outside the writer; version
  validation and atomic publication with head deletion and frontier advancement.
- At most 32 independently decoded blocks per group, 240 samples per block,
  event-span cap, inline small bodies, no body for constants, separate larger
  payloads and immutable allocation/live masks.
- Shared clock objects with hash plus equality checks, ownership updates in the
  write transaction, and an eight-entry query-local descriptor cache. Irregular
  clocks compare quantum-scaled deltas, runs and dominant-step exceptions.
- Value candidates: original integer/scaled/XOR/raw codec, change events, exact
  decimal grid with IEEE residuals. Grid scale search is remembered per series.
- Compact exact summaries including counter resets/increase. Invalid numeric
  aggregates are marked unusable; the public API currently returns raw samples.
- Retention of quiet heads and partial groups, stable remaining payload ids,
  last-owner clock deletion, and correct due scheduling after series reactivation.
- File identity, checksummed migration history, full-synchronous WAL writes,
  one writer, two query-only readers, and ordered close.

Schema version two opens version-one files; old payload/directory readers remain.
Golden vectors pin both directories and the clock format. The new formats are
unreleased and are intentionally distinct from the experimental spike parsers.

## Verification

- Windows: all normal tests with shuffled order, golangci-lint, formatting,
  both module tidy checks and govulncheck passed.
- Linux Docker: `go test -race -shuffle=on ./...` passed.
- Linux fuzz run for the new clock/value/directory/residual parsers: 15 seconds,
  approximately 119450 executions, no failure. Fuzzing is finite evidence, not
  a proof against every malformed input.
- Publication fault injection rolls back already-written payloads, head
  deletions, ids and group changes. Reopen after a subprocess exits without
  Close preserves committed head and sealed data.
- Bit tests cover signed zero, NaN payloads, infinities, arbitrary float bits,
  changes, grids and exact residual streams. Tests also cover concurrent
  ingestion/packing/expiry with readers, snapshot completeness, limits, stale
  candidates, partial expiry and shared-clock ownership.
- A CGO-free linux/amd64 stripped probe that calls the public engine links to
  8175776 bytes, 6668288 bytes above the empty baseline on this toolchain.

## Real-engine corpus checks

Go 1.27.1, linux/amd64 Docker on Ryzen 7 7700, modernc SQLite 1.59.0,
klauspost/compress 1.19.0. Same normalized Alibaba/TSBS inputs as the research
reports. No competitor was rerun here.

`TestCorpusThroughPublicStore` writes through public Ingest in chunks of 7681
samples, runs Maintain after each chunk, closes the database, measures the file,
reopens it and reads every series through public Read with bitwise comparison.
Retention is set to 100 years to preserve the historical corpus; max head/batch
samples are 8192. Default lateness is zero; the remaining short safe prefix and
newest point stay in the mutable head, as the normal API promises.

| Corpus | Samples | Complete file bytes | B/sample | Full reopened bit check |
|---|---:|---:|---:|---|
| Alibaba | 12431885 | 9113600 | 0.733083 | passed |
| TSBS | 5090400 | 12775424 | 2.509709 | passed |

These are **not** the compacted-only research numbers. The real file includes
mutable heads, postings, migration history and durable state. On TSBS there
are 242400 head samples across 2020 series. Their table alone uses 6402048 bytes;
groups use 409600, payloads 3076096, postings 663552, registry plus unique index
2023424. Exactly one clock object serves all its groups. The row-head and registry
costs are visible rather than silently omitted from the engine result.

Diagnostic ingest+maintenance+input-processing time was about 73 s on Alibaba
and 41 s on TSBS; reopened parse+read+bit-check about 3.1 s and 2.5 s. The runs
were not isolated throughput benchmarks, and include durable transactions and
maintenance. No performance win is inferred from these elapsed times.

Reproduce after preparing a normalized corpus with the existing bench runners:

```powershell
$env:TINYSTORE_JSONL = '<corpus-directory>/series.jsonl'
go test ./metrics -run '^TestCorpusThroughPublicStore$' -v -count=1
```

Use the existing Linux container/mounted corpus setup for numbers comparable
to the research reports. The test skips without the environment variable.

## What is still work

A packed mutable head is the clearest remaining density task. Group merging,
automatic grid retraining, byte/page-aware closure and aggregation API semantics
also remain. Maintenance is explicit, not a background scheduler. The reader
uses a dedicated bounded codec separate from the writer; multiple independent
decoder workers have not been added without an RSS/concurrency measurement.

The crash probe tests abrupt exit after committed operations, not injected
power loss or interruption at every instruction in COMMIT. Steady-state
WAL/RSS, cold queries, p95/p99 and long retention/reuse remain acceptance work.
These limitations do not block the executable first slice, but they do block
advertising the earlier 0.3608 B/sample as its current production density.
