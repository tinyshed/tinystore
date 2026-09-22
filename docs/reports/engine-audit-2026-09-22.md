# Engine audit: remaining work and misleading instruments

Code inspected: `f2974bc`. This is an audit, not another engine implementation.
Production code was not changed. Findings below distinguish reproduced behavior,
direct code observations, profile evidence and optimizations still requiring a
comparison. No new compression ratio or projected speedup is claimed.

## Evidence and environment

Six diagnostic probes ran on Windows 11, Go 1.27.1 windows/amd64. Their sources
are under [engine-audit-2026-09-22-probes](engine-audit-2026-09-22-probes/).
They assert the observed limitations, not the desired future behavior, and are
kept outside the normal production test packages. Go overlays make them runnable
without modifying engine files:

```powershell
pwsh -NoProfile -File <repo>/docs/reports/engine-audit-2026-09-22-probes/run.ps1
```

A separate profiling/observer check ran on Ryzen 7 7700, Docker Desktop on
Windows 11, Linux 6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs, Go 1.27.1
linux/amd64, modernc SQLite 1.59.0, libc 1.75.7, memory 1.12.1 and
klauspost/compress 1.19.0. The database was on the `tsperf` named volume.
The existing `s10k` fixture has 10,000 series × 500 samples, SHA-256:

```text
4d41358bd6c0854baffba05829406090559237803390bd8e12eac4c2da44c436
```

The query selects one fixed series, a randomized one-hour window and 360 samples.
Runs were sequential and warm. Observer comparisons lasted three seconds;
the separate CPU/mutex profile lasted five seconds. That profile's throughput
must not be compared with the unprofiled rows as an optimization result.

| readers | qps with observer | qps without observer |
|---:|---:|---:|
| 1 | 8,431.4 | 8,415.6 |
| 8 | 9,646.1 | 9,658.9 |

Disabling the periodic heap/file sampler did not explain this workload's
scaling plateau. This does not validate the counters it reports.

| CPU stack, cumulative | sampled CPU share |
|---|---:|
| fetchSnapshot | 51.56% |
| matchSeries, included in fetchSnapshot | 36.16% |
| rankMatchers, included in matchSeries | 16.74% |
| fillLabels, included in matchSeries | 7.61% |
| decodeResults | 9.92% |

These percentages overlap and must not be added. Codec.Decode accounted for
2.95% of sampled mutex delay; this profile does not identify its shared mutex
as the main bottleneck. The immediate read-side target is registry work and SQL
execution frequency, not another value codec.

Reproduce using the populated fixture from the performance report:

```powershell
docker run --rm -v <repo>:/src -v tsperf:/perf `
  -v tinystore-gocache:/go -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache `
  -e GOWORK=off golang:1.27 sh `
  /src/docs/reports/engine-audit-2026-09-22-probes/observer.sh `
  /src /perf/audit-new /perf/s10k
```

`/perf/audit-new` must not exist. The script modifies an archived copy only.

## 1. Instrumentation needs correction before more tuning

**WAL writes are not measured.** `watch` in `bench/perf/main.go` adds positive
changes in file length to `walWritten`. Reusing existing WAL space does not
increase file length; the 20 ms sampler also misses changes between observations.
`wal_bytes_per_sample` is therefore observed file growth, not WAL bytes written,
fsync count or write amplification. SQLite explicitly describes WAL rewind and
reuse in its [WAL documentation](https://www.sqlite.org/wal.html#how_wal_works).
Use instrumented writes/frame generations for write volume and keep peak file
size as a separate metric.

**Go Sys is not process RSS.** `peak_process_mib` comes from `MemStats.Sys`.
In the pinned Linux dependencies, libc's `mem_musl.go:Xmalloc` calls
`memory.Allocator.UintptrMalloc`; `memory/mmap_unix.go` uses anonymous
`unix.MmapPtr`. Pure Go does not put these allocations inside the Go heap.
`HeapAlloc`, Go allocation counts and Sys remain Go-runtime statistics; none
establish total process residency. The earlier claim that modernc's page cache
must be covered because it is pure Go is wrong. Measure OS RSS separately.

**The cardinality write test conflates registration and ingestion.** The
100k-series stage gives each series only 20 samples and fills a 10,000-sample
batch series by series: 500 newly registered series per call, each touched once.
Its slowdown cannot establish that repeated head rewrites caused it. Repeated
rewrites exist in the engine, but that workload does not isolate them. Separate
registration, append-to-existing-series and steady sealing workloads.

The same harness repeatedly calls `countSamples(batches)` while building a batch,
making generation quadratic in the number of series in that batch. Its overall
allocation and throughput counters also include fixture/bookkeeping work.
The mixed sweep mutates the same database between reader-count stages, so its
rows do not start with identical heads and group histories. Fixed-key warm reads
also need a separate rotating-key/working-set test. None of these observations
invalidates a directly measured file size or elapsed time; they constrain its
interpretation.

## 2. Limits on each operation do not bound all active work

`Store.enter` counts operations for Close but does not limit admission.
`MaxReaders` limits snapshots; their bytes remain alive during decode after the
connection is released. Concurrent Ingest calls prepare maps and slices before
waiting for the single writer. Enough individually valid calls can therefore
exceed a process memory target.

Add bounded work admission or weighted memory reservations before preparation,
held until decoding/encoding completes. Keep the short SQLite transaction. This
is especially important when several future engines share one process. The
current per-query limits are real; they are not an aggregate memory guarantee.

## 3. Retention does not reclaim cardinality

Reproduced: `MaxSeries=1`, expire the only series completely, then ingest a new
label set. It still fails with ErrLimit. `expireSeries` removes samples, while
series, postings and dictionary entries remain; `series_count` only increases.
This behavior is documented, not an undisclosed raw-data corruption bug.

It is nevertheless a major deployment limit for container/pod/process churn.
Define explicit series deletion or bounded empty-series reclamation, including
kind identity, frontier safety, dictionary ownership and future handle reuse.
Do not equate registered lifetime cardinality with currently active series.

## 4. Maintenance selection can starve later series

Reproduced with `MaintenanceSeries=1`: two series are ready; after each pass,
replenish series 1. Five passes seal only series 1; series 2 never gets a frontier.
`dueSeries` always starts with the smallest ready series_id. The pattern scales
to the default batch width. A hot prefix can keep later heads waiting until
their configured capacity starts rejecting ingestion.

Use bounded fair selection, such as a persisted round-robin cursor or an indexed
queue age. Also distinguish total head count from watermark-eligible count:
currently 240 points can repeatedly set ready even when lateness prevents sealing,
causing another read and a clearReady write on the next maintenance pass.

## 5. Selectivity ranking has both a fixed cost and a blind spot

Reproduced: posting lists of 2,048 and 1,025 entries both rank as 1,024; stable
sorting retains the broader, alphabetically earlier matcher. The cap bounds
probing but does not guarantee the shortest driving list. The flat point-read
curve established for a 20-series host posting does not cover this case.

Maintain posting cardinalities transactionally on series registration/deletion,
or use a bounded adaptive intersection strategy. Reading counters would remove
the per-query row-count probes and distinguish large lists. One matcher needs
no ranking at all. Resolve multiple matcher ids in bounded batches rather than
one SQL execution per label. The fresh profile makes this a first-priority
optimization, with a test where every posting exceeds the current cap.

## 6. Raw reading still performs many tiny SQL operations

`fetchSnapshot` traverses matched series one by one. Each gets a group query and
a head query, then each selected external block gets another payload query.
Prepared statements remove parsing, not binding, rows objects, cancellation
machinery and Go/driver calls.

Fetch bounded batches of heads and group descriptors; identify the selected
payloads; fetch bounded id batches after checking the budgets. Preserve one
snapshot and avoid speculative loading of unrelated payloads. A registered
series handle or an explicit exact-identity read could bypass postings when
the caller already identifies a whole series; it must carry store/lifetime
identity rather than exposing a reusable bare integer.

## 7. Label reconstruction does work that its caller discards

`canonicalLabels` sorts, validates and serializes JSON. Read discards its JSON
result; `fillLabels` does it again for every matched series and discards that JSON
too. Separate canonical validation/order from identity serialization. On ingest,
the existing-series path also serializes the legacy label shape and resolves
dictionary ids again to prove identity. Keep the full-label collision check,
but move repeated identity resolution behind a bounded cache or a typed handle.

## 8. Ingestion rewrites the entire mutable head

`writeHead` loads and decodes all old points, merges into another slice, then
`encodeHead` runs the codec on every chunk. One appended point can revisit a
large late-arrival window while holding the writer transaction.

The format already contains independent chunks. Reuse encoded chunks outside
the changed timestamp interval and only decode/re-encode intersecting chunks;
an append usually touches the last chunk. This changes CPU work without moving
the sealed frontier. A single tail BLOB still incurs physical rewrite costs;
separate chunk rows would be a later layout comparison, not a free consequence.

`prepareIngest` also builds a timestamp map and sorts even sorted unique input.
Provide an ordered fast path with the same duplicate-last-wins fallback. These
are more promising algorithmic targets than micro-optimizing varint instructions.

## 9. Writer SQL and commit granularity remain expensive

The current compact existing-series path normally executes seven statements:
identity lookup, label-id verification, ingest-state read, head read, head update,
legacy-head delete, and another state update. Writer statements are not cached.
Combine compatible state reads/updates and avoid deleting nonexistent legacy
rows once their absence is known. Add bounded writer statement reuse.

Each published series in Maintain gets its own transaction; expiry and clearReady
also transact separately. Stage a small, byte-bounded number of encoded candidates
outside the writer, then publish them in one transaction with version checks and
correct rollback accounting. Avoid buffering an arbitrarily configured
MaintenanceSeries worth of full heads. FULL durability stays unchanged.

`expireSeries` additionally decodes the head even when only an old sealed group
is due. Head bounds can decide whether that decode is needed. `refreshDue` loads
clock/directory metadata to obtain one live block's end; a lightweight validated
descriptor path could serve it and raw reads alike.

## 10. The prepared-reader cache has avoidable churn

Two probes reproduced issues introduced with the prepared-reader wrapper:

- Four sequential queries with MaxReaders=4 open all four connections, because
  returning a slot to the channel cycles through untouched slots. Prefer reuse
  of warm idle connections and allocate new ones only under contention.
- Any callback error discards the entire connection and statement cache, including
  an ordinary application budget error. Distinguish unusable/cancelled connections
  from a healthy connection after a normal rollback.

FIFO statement eviction also competes with many IN-list and matcher-count SQL
shapes. Normalize bounded batch sizes or reserve a small fixed set of hot programs
before making the cache larger. Its existing 32-entry cap should remain explicit.

## 11. Compression and read granularity need separate budgets

Merging 32 blocks saves storage and large-read work, but a point read still
parses a whole compressed directory. A metadata-only raw descriptor path, offsets
or smaller independently decoded directory sections are candidates. No block
summary arithmetic is needed merely to return raw samples.

The eight-clock cache is traversed series-first. A range covering more than eight
shared clocks can cycle through all of them again for every series. Grouping work
by clock or an explicit metadata cache budget should be compared before simply
raising the cache count. Irregular clocks are validated by expansion and selected
ones are expanded again during decoding; validation can walk checked deltas
without retaining a timestamp array.

The output path also constructs timestamp/delta arrays, temporary decoded sample
slices and a growing result slice. The ordinary-value adapter constructs a codec
envelope/checksum that Codec.Decode immediately verifies again. Reusable bounded
decode scratch and an append-to-output path can reduce copying. The profile does
not currently justify making an unbounded decoder pool.

## 12. Aggregates need a numerical contract before a summary shortcut

There is a real opportunity to answer whole blocks from existing summaries and
decode only clipped edges. But combining ordinary float64 block sums does not
necessarily equal summing raw samples in time order.

Reproduced with 480 values: block 1 begins `1e16, 1`, block 2 begins `-1e16, 1`,
all remaining values zero. Sequential sum is **1**; adding the two stored block
sums gives **0**. This is a future-API constraint, not a wrong current raw Read.
Define whether sums promise sequential IEEE rounding, a reproducible mergeable
rule, or correctly rounded mathematical sums; the latter two may require a
different summary representation. Counter increase also needs the transition
between adjacent blocks, not just the sum of their internal increases.

## 13. Remaining density work should follow the file's actual objects

- A failed first nonconstant grid search persists hint=-1 and later blocks never
  retry. Startup NaNs or an early noisy phase can exclude a useful model for a
  long-lived series. Bounded periodic retraining is a format-compatible candidate.
- The identity remains a 44-byte textual digest. A full binary digest is a simpler
  first comparison than truncation; the current unique index and ErrConflict
  collision path do not implement a multi-entry collision bucket.
- Fixed group width and inline threshold ignore actual SQLite row packing.
  Compare byte-aware grouping/inline choices using per-object dbstat, point reads
  and wide reads. More compression in a blob does not ensure fewer database pages.
- Clock objects include absolute starts, limiting sharing across offset scrapes.
  Relative clock patterns are only worth a new version if clocks materially
  contribute to the measured file.
- Shared Huffman tables and sibling prediction remain research-only. They must
  pay for lookup, references, retention and decode budgets; earlier payload-only
  gains are not promises about engine size.

## Architecture assessment and next sequence

Metrics, events/records, KV, application SQL and blobs can share file mechanics,
migrations, prepared programs, bounded work admission and diagnostics. Their
schemas, lifecycle rules and payload formats should remain separate. Application
SQL should have an explicit data boundary rather than bypassing metrics-owned
invariants. Blob streams need file-generation leases after the metadata snapshot
ends. See [the runtime assessment](../storage-runtime-direction.md).

SQLite is still a reasonable foundation. A database has one simultaneous writer;
WAL requires same-host coordination and transactions across attached WAL databases
are not atomic as a set. These are documented boundaries, not findings that our
current workload has reached SQLite's ceiling. See
[SQLite WAL](https://www.sqlite.org/wal.html) and
[appropriate SQLite uses](https://www.sqlite.org/whentouse.html).

Recommended order:

1. Correct RSS/WAL labels and split registration, ingestion and mixed benchmarks.
2. Close the operational gaps: fair maintenance, churn policy and active-work bounds.
3. Remove query-time posting counts and discarded label serialization; measure
   rotating-key reads, wide reads and selectors whose every posting exceeds 1024.
4. Reuse head chunks, writer statements and small publication batches; compare
   writer wait/hold time, fsyncs and actual WAL writes.
5. Add aggregate semantics and edge tests, then summary-driven execution.
6. Use per-object/per-field census to choose the next density change.

Only the diagnostic probes and targeted profiling were run for this audit;
the full product suite was not rerun because production code did not change.
