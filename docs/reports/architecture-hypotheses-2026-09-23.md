# Execution, churn and layout hypotheses after the saved-revision benchmark

This round records WP9, WP10, WP12 and the ordinary-value envelope gate from
the [architecture worklist](architecture-worklist-2026-09-22.md). It follows
the physical-write and throughput baselines in the
[final benchmark](final-architecture-benchmark-2026-09-23.md). Engine source is
saved as `4589144`, writer instrumentation as `aecd7b2`, and the benchmark
harness as `dd4a6e5` on `codex/architecture-measurements`. The benchmark-only
proxy and direct-binding dependencies live in `bench/perf`, not in root
`go.mod`.

## Environment

Ryzen 7 7700, Windows 11, Docker Desktop 29.6.2, Linux amd64 container with
16 visible CPUs, Go 1.27.1, modernc SQLite 1.59.0, data on the named Docker
volume `tsperf`. Each timed stage ran sequentially. The raw comparison used
10,000 primary-key rows with 200-byte payloads, 100,000 pseudorandom lookups
per mode and 1 MiB page caches; every mode checks the CRC32 of all 200 fetched
bytes. The `zombiezen.com/go/sqlite` proxy was v1.4.2, but Go selected
modernc SQLite 1.59.0 for the benchmark module as for the engine.

## WP9: execution and writer settings

An ABBA-like sequence on one file, with the SQL and `sql.Conn.Raw` paths on one
connection and proxy on a second, produced:

| Path | ns/lookup, two positions | allocations/lookup |
|---|---:|---:|
| prepared `database/sql` | 10,831; 12,748 | 31.97–31.99 |
| prepared driver statement inside `sql.Conn.Raw` | 8,974; 10,801 | 13.98 |
| direct modernc translated SQLite binding | 7,770; 9,106 | 0 reported |
| zombiezen proxy | 7,561; 7,836 | 0 reported |

The paths differ in cancellation behavior. `database/sql` and `Raw` call the
driver with a per-statement context. The proxy has one connection-level
interrupt. The direct experiment uses reflection to reach modernc's unexported
handle and does not implement cancellation of an in-flight statement. It is
not a production design. The sequence also drifted as it ran, so the table
supports a bounded microbenchmark ranking, not a projected engine QPS gain.
No execution-layer replacement is approved: a candidate must beat the batched
real `Read` path while preserving snapshot ownership, cancellation, cleanup
and connection reuse. The root module remains on `database/sql` plus modernc.

Writer cache size was varied with a Go build overlay changing only
`cache_size(-1024)` to `cache_size(-4096)`. On the same 10,000-series fixture,
100,000 timed appends in 1,000 calls gave 17,001 and 17,102 samples/s at
1 MiB, versus 16,968 and 16,907 at 4 MiB. Both produced 3,964,928-byte
closed files. Keep 1 MiB; the larger cache did not improve this workload.

The controlled 4 KiB versus 8 KiB page test in the
[final benchmark](final-architecture-benchmark-2026-09-23.md) found fewer
CacheWrite pages at 8 KiB but more WAL bytes and larger closed files for
registration, append and sealing. Keep 4 KiB. Actual WAL frames, checkpoint
copy counts and successful Linux `fsync` calls were measured there; sampled
WAL file growth in older reports remains a different counter.

Reproduce the lookup run by building and running `bench/perf/raw` on Linux;
its one command prepares the file and runs all four modes in fixed order:

```sh
go -C bench/perf run ./raw <volume>/raw
```

For the cache test, build `bench/perf/repro` normally and with an overlay
that replaces only `cache_size(-1024)` in `internal/sqlite/file.go` with
`cache_size(-4096)`, then run each binary twice in 1/4/4/1 order:

```sh
<repro> -dir <new-volume-directory> -stage append -series 10000 -rounds 10 -batch 100
```

## WP10: sustained rotating cardinality

`bench/perf -stage long_churn -series 1000 -epochs 30` registered and fully
reclaimed 30,000 short-lived series over 30 epochs. The deterministic labels
rotate host identity while reusing other label pairs; it is a synthetic
container-like cardinality workload, not the private Telegraf capture. Each
epoch waits for the one-second retention cutoff, then calls bounded `Maintain`
until every series is reclaimed. The wait is excluded from ingest and reclaim
timings.

| Measure | Result |
|---|---:|
| Ingest epoch p50 / p99 | 58.1 / 64.6 ms |
| First five / last five ingest mean | 58.0 / 59.0 ms |
| Reclaim epoch p50 / p99 | 3.96 / 6.26 s |
| First five / last five reclaim mean | 3.92 / 3.91 s |
| Largest open file + WAL + SHM | 4,690,288 B |
| Closed file after 30 epochs | 380,928 B |
| Final free pages / total pages | 75 / 93 |

Every remaining `dbstat` b-tree, including `series`, postings, dictionary and
state, occupied one 4 KiB page after complete reclamation. The closed file
does not shrink to its 18 live b-tree pages: 75 pages are free for reuse.
There is no increasing first-to-last epoch latency in this 30-epoch run, but
roughly four seconds to reclaim 1,000 series is a material maintenance cost.
Do not infer multi-day behavior or a million-series service rate from this
synthetic trace.

A second run selected 1,000 evenly spaced identities from the complete
Telegraf capture (SHA-256
`6475b9d3dcf8cf2ce88de5084ea37981d9d5676cdf68086b89115fb1d808ad0d`).
Each epoch kept their real label sets and first numeric values but suffixed
the `host` value with that epoch, so identities rotate without changing label
count. This is a **real-label-derived rotating workload**, not 20 observed
days of a live agent. Twenty epochs registered and reclaimed 20,000 series:

| Measure | Real-label-derived result |
|---|---:|
| Ingest epoch p50 / p99 | 77.1 / 83.6 ms |
| First five / last five ingest mean | 77.2 / 80.5 ms |
| Reclaim epoch p50 / p99 | 3.75 / 3.87 s |
| First five / last five reclaim mean | 3.79 / 3.79 s |
| Largest open file + WAL + SHM | 4,920,216 B |
| Closed file | 516,096 B |
| Final free / total pages | 108 / 126 |

Every final b-tree again occupies one page and the rest are free for reuse.
The reclaim cost stays flat over these cohorts, while registration rises by
about 4% from the first to last five epochs. That small rise is a finding to
watch, not proof of unbounded degradation. A genuine multi-day capture of
container creation and deletion remains the stronger operational gate.

```sh
<perf> -dir <new-volume-directory> -stage long_churn -series 1000 -epochs 20 \
  -corpus <corpus>/telegraf/series.jsonl
```

## WP12: current-corpus digest and column layout

The same complete 4,435-series, 4,939,641-sample Telegraf file and the
2,020-series, 5,090,400-sample TSBS file were each copied three ways. Every
candidate was vacuumed before page accounting, and `pragma foreign_key_check`
and `integrity_check` passed. The candidate replaces the textual 32-byte
SHA-256 digest's 44-character base64 column with its 32-byte binary form and
a **non-unique** lookup index; the experiment retains full label IDs so
collisions could be resolved by full comparison in a real migration. It is a
file-layout probe, not a changed production schema. Corpus SHA-256 values are
in the [final benchmark](final-architecture-benchmark-2026-09-23.md).

| Corpus | Vacuumed current file | Binary-digest candidate | Series b-tree before → after | Identity index before → after |
|---|---:|---:|---:|---:|
| TSBS | 4,366,336 B | 4,317,184 B | 155,648 → 131,072 B | 110,592 → 86,016 B |
| Telegraf | 4,845,568 B | 4,739,072 B | 323,584 → 270,336 B | 237,568 → 184,320 B |

The 49,152 and 106,496 bytes saved are 1.1% and 2.2% of these whole files,
respectively. On 100,000 warm identity lookups, the TSBS text path took
4.94–5.17 µs and binary 5.04–5.06 µs; Telegraf text took 5.13–5.14 µs
and binary 5.23–5.30 µs. The binary form did not buy a lookup speedup.
Given its migration and collision-bucket obligations, keep the current digest
for now.

Moving `series_state.tail` to the final table column produced exactly the
same vacuumed file size for both corpora. On 100,000 random `head_start` and
`head_end` lookups, TSBS current layout took 4.43–4.47 µs and tail-last took
4.50–4.51 µs; Telegraf current took 4.49–4.50 µs and tail-last 4.53 µs.
Do not rebuild the state table for this hypothesis.

Reproduce each candidate on a copy of the prepared public-store database;
never mutate the original corpus file:

```sh
go -C bench/perf run ./layout -db <volume>/baseline.db -mode vacuum
go -C bench/perf run ./layout -db <volume>/binary.db -mode binary_digest
go -C bench/perf run ./layout -db <volume>/tail.db -mode tail_last
go -C bench/perf run ./layout -db <volume>/baseline.db -other <volume>/binary.db -mode identity
go -C bench/perf run ./layout -db <volume>/baseline.db -other <volume>/tail.db -mode bounds
```

## WP5: redundant ordinary-value envelope

`BenchmarkOrdinaryEnvelope` isolated the ordinary 240-sample decoder on a
noisy-float block. Across three one-second Linux runs, rebuilding the temporary
envelope and checksum took 10.29–10.33 µs and 9 allocations / 8,032 B per
block. Reusing a prebuilt checked envelope in the benchmark took 9.65–9.77 µs
and 4 allocations / 6,176 B. This bounds the isolated envelope opportunity at
about 0.6 µs and 1.8 KiB per block on this input; a full `Read` still pays
matching, SQL, payload fetch, decoding and output construction. Do not add an
unchecked production decoder for this gain. A future bounded, already-verified
value-stream entry point would need corruption and decode-budget tests.

```sh
go test ./metrics -run '^$' -bench '^BenchmarkOrdinaryEnvelope$' -benchmem -benchtime=1s -count=3
```

## WP3: variable SQL shapes

`lookupLabelIDs` forms one prepared SQL program per label count. A benchmark
used the actual label pairs from both normalized corpora against their
persisted `label_values` tables, a 32-program recently-used statement cache,
20,000 rotating lookups per path and identical result counts. The candidate
replaced the dynamic `VALUES` list with one `json_each(?)` join. On Linux:

| Corpus | Dynamic VALUES, ns/lookup | JSON list, ns/lookup | Cache evictions during timed lookups |
|---|---:|---:|---:|
| TSBS | 15,045–15,113 | 19,524–19,893 | 0 in either path |
| Telegraf | 14,519–14,610 | 17,624–18,301 | 0 in either path |

The actual public-store corpus runs gave writer-cache totals of 15 prepared
programs, zero evictions and 4,041 commits for TSBS; 63 preparations, 31
evictions and 8,871 commits for Telegraf. Telegraf therefore does see a small
amount of churn when its other writer programs share the cache, but it is not
the dominant work in a 35.7-second ingest/maintenance pass. A synthetic
63-distinct-label-count stress test prepared and evicted on all 20,000 dynamic
lookups, taking 53.0 µs each; the one-shape JSON path took 46.3–47.4 µs with
no eviction. That stress input is deliberately outside either measured corpus.

Keep the dynamic SQL on the production path. The JSON candidate loses on both
real corpora, while the observed 31 real evictions do not justify its recurring
parse cost. If future installations show frequent 32-program churn, a measured
hybrid or separate cache policy may be justified; the current result does not
prove that more cache entries would be free in memory.

```sh
go -C bench/perf run ./sqlshape -db <volume>/tsbs.db -corpus <corpus>/tsbs/series.jsonl -operations 20000
go -C bench/perf run ./sqlshape -db <volume>/telegraf.db -corpus <corpus>/telegraf/series.jsonl -operations 20000
go -C bench/perf run ./sqlshape -db <volume>/telegraf.db -synthetic-shapes 63 -operations 20000
```

## Still open

Native macOS CI has not run because the GitHub Actions quota was exhausted;
the user waived that run. Darwin amd64 and arm64 CGO-free cross-builds and
metrics test-binary compilations passed, but those are not macOS runtime or
race tests. The new shared work
budget and exact aggregate contract have separate reports and gates.
