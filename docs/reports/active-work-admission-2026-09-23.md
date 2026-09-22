# Bound active read decoding and ingest preparation per store

This is the admission part of WP8, separate from a group-commit actor or
checkpoint policy. `Read` acquires a store-level slot before preparing the
request and holds it through snapshot fetch and decode. `Ingest` acquires a
separate slot before `prepareIngest` and holds it through commit. Defaults are
`MaxConcurrentReads=MaxReaders` and `MaxConcurrentIngest=1`; both can be set
independently. Waiting honors the caller's context. Maintenance already has
one serialized pass and a bounded publication staging budget.

This bounds the number of simultaneously active requests *on one Store handle*.
It does not prove a total process RSS ceiling: Go and SQLite overhead are not
part of the query's byte budget, and separate stores do not share these slots.
`TestActiveReadAndIngestAdmissionHonorsCancellation` checks both waits and
successful work after the held slots are released.

## Environment and input

Ryzen 7 7700, Windows 11, Docker Desktop,
Linux 6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs,
Go 1.27.1 linux/amd64, modernc SQLite 1.59.0. The database lived on Docker
volume `tsperf`. Each stage copied the same 10,000-series × 500-sample
`arch-s10k/read.db`, SHA-256
`cdf4a070790d6bd98865b68dc5f3ee46f8adae3f5b45a5b2b11835980301405d`.
The engine was the uncommitted `codex/architecture-measurements` working tree
above `f4568cc`, including the batched read path. Eight goroutines repeatedly
read a one-hour window for 2,500 series, 900,000 returned samples per query.

Five-second runs varied only `MaxConcurrentReads`, in the order 8, 2, 2, 8:

| active-read limit | QPS | peak OS RSS | p50 request latency |
|---:|---|---|---|
| 8 | 42.6; 40.7 | 356.5; 346.9 MiB | 177; 187 ms |
| 2 | 25.1; 24.9 | 108.4; 112.3 MiB | 311; 309 ms |

Limiting the active work to two reduced observed peak RSS by about **68%** on
this workload, while throughput fell about **40%** and queued request latency
grew. That is a capacity choice, not a universal default improvement. With
the default active limit equal to eight readers, the new code's 40.7–42.6 QPS
overlaps the 40.8–41.2 QPS measured immediately before admission was added.
The RSS values are 20-ms samples of `/proc/self/statm`; shorter peaks may be
missed. They are process values for these runs, not a formal bound derived
from the configured query limits.

## Reproduce and checks

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-admission .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-admission -dir /perf/arch-s10k -stage read \
  -series 10000 -shape selector_low_cardinality -readers 8 \
  -active-reads 2 -seconds 5
go test ./metrics -run TestActiveReadAndIngestAdmissionHonorsCancellation -count=1 -v
```

The final `task check` and Linux race runs are recorded in
[the worklist](architecture-worklist-2026-09-22.md). Cross-store weighted
admission, group commit of independent requests and checkpoint policy remain
open design and measurement work.
