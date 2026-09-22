# Batched head reads reserve bytes before fetching bodies

This is the first WP5 read-path experiment. For at least 16 matched series,
the reader fetches head counts, time bounds and BLOB lengths in bounded batches
of 64. It charges the decoded-sample and payload-byte budgets before a second
query fetches only selected packed tails. Legacy heads still use their existing
reader, and smaller matches keep the original per-series path. The descriptor
and body queries use fixed SQL text through the bounded, connection-owned
prepared-reader cache; both run in the same snapshot as groups and postings.

`TestBatchedHeadsPreserveSnapshotAndReserveBytesFirst` reads 20 series across
sealed blocks and mutable tails with exact float bits, then gives the batch
reader too little byte budget and verifies it never requests tail BLOBs.

## Environment and comparison

Ryzen 7 7700, Windows 11, Docker Desktop,
Linux 6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs,
Go 1.27.1 linux/amd64, modernc SQLite 1.59.0. The database lived on Docker
volume `tsperf`. Each stage copied the pinned 10,000-series × 500-sample
`arch-s10k/read.db`, SHA-256
`cdf4a070790d6bd98865b68dc5f3ee46f8adae3f5b45a5b2b11835980301405d`.
Both binaries included the hybrid matcher path; only batched head fetching
changed. The source states on `codex/architecture-measurements` above `f4568cc`
are uncommitted, so the comparison is provisional.

One-hour queries returned 360 samples per series. Six-second runs for the
2,500-series region were old, new, new, old; five-second runs for the 20-series
host used the same order:

| selector | old QPS | new QPS | old → new allocs/query |
|---|---|---|---:|
| 2,500 series | 10.4; 10.4 | 11.3; 11.0 | ~554,650 → ~467,790 |
| 20 series | 1,156; 1,168 | 1,206; 1,183 | ~4,878 → ~4,220 |

The two-run means improved about **7%** and **3%**, respectively. The large
query still executes group and external-payload SQL per series; this result
does not support the review's expected 20–28 QPS for fully batched reads.
Point and single-series requests do not enter this new path.

## Reproduce and checks

The old binary `/perf/perf-arch-matcher-hybrid` and candidate
`/perf/perf-arch-headbatch` are retained in `tsperf`; their source states
have not been committed. Build and run the candidate with:

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-headbatch .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-headbatch -dir /perf/arch-s10k -stage read \
  -series 10000 -shape selector_low_cardinality -readers 1 -seconds 6
go test ./metrics -run TestBatchedHeadsPreserveSnapshotAndReserveBytesFirst -count=1 -v
```

`task check` passed on Windows, including the full shuffled suite, lint,
vulnerability scan and size report. `go test -race ./metrics` passed in the
Go 1.27.1 Linux container. Descriptor and selected-payload batching remain
open; this round does not change their budgets or SQL count.
