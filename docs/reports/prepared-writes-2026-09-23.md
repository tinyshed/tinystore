# Prepared writes on the single owning connection

This implements the bounded writer-statement reuse part of WP6. `File` keeps
one pinned SQLite writer connection behind context-aware single-writer
admission. Its prepared-program cache is bounded to 32 SQL texts and uses the
same owning-connection mechanics as prepared reads. `Ingest` executes its
registry lookups and state writes through that connection inside one immediate
transaction. Other write operations still use their `*sql.Tx` callback on the
same pinned connection. No program passes through `Tx.StmtContext`.

`TestPreparedWriterUsesOneTransactionAndRetainsPrograms` verifies that a
failed callback rolls back its prepared insert, a later callback reuses the
same statement, and only the committed row is visible. A second test exercises
96 SQL texts and confirms the 32-program cap. Driver-bad, cancelled and
uncertain transactions discard the connection and its statements before reuse.
SQLite remains single-writer and `synchronous=FULL` is unchanged.

## Environment and input

Ryzen 7 7700, Windows 11, Docker Desktop,
Linux 6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs,
Go 1.27.1 linux/amd64, modernc SQLite 1.59.0. Fresh database files were on
Docker volume `tsperf`. Both binaries used the same `bench/perf` harness,
schema and preceding engine changes on the uncommitted
`codex/architecture-measurements` branch above `f4568cc`.

The append stage seeded 1,000 existing series, then timed 50 new samples per
series in calls touching 100 series, without maintenance. The registration
stage timed one sample in each of 10,000 new series, 100 series per call.
Each comparison ran old, new, new, old sequentially on fresh files:

| stage | old samples/s | prepared-writer samples/s | allocs/sample, old → new |
|---|---|---|---:|
| append | 8,215; 8,377 | 13,798; 14,239 | 212.3 → 209.2 |
| registration | 4,453; 4,513 | 8,963; 9,114 | 298.5 → 279.9 |

The two-run mean throughput improved about **69%** for append and **101%** for
registration. This is the strongest measured write-path change in these
rounds, but applies to the named batch size, series shape and machine. The
closed files were byte-identical in size within each stage pair: 405,504 B
for append and 3,158,016 B for registration. Sampled WAL file growth was
52.82 and 472.15 B/sample, respectively, in both variants. Those numbers
do not measure frames written, fsyncs or device write amplification.

## Reproduce and checks

The old binary `/perf/perf-arch-ordered` and new binary
`/perf/perf-arch-writercache` are retained on `tsperf`; the source states are
not yet committed. Build and run the current candidate with:

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-writercache .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-writercache -dir /perf/arch-writer-new -stage append \
  -series 1000 -seed-samples 1 -samples 50 -batch 100
go test ./internal/sqlite -run 'TestPreparedWriterUsesOneTransactionAndRetainsPrograms|TestPreparedWriterCacheStaysBounded' -count=1 -v
```

The full branch `task check` and Linux race runs are recorded in
[the worklist](architecture-worklist-2026-09-22.md). Direct SQLite execution,
`CacheWrite` and sync counters, commit batching and group-commit actors remain
separate proposals; this result alone does not justify them.
