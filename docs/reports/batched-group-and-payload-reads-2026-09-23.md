# Batch selected payloads and group directories

This continues WP5 after [batched heads](batched-head-reads-2026-09-23.md).
For matches of at least 16 series, the reader reserves each selected external
payload's bytes while decoding its group directory, then fetches payloads in
batches of 64 identifiers. Group directories are also fetched in two phases:
bounded descriptors and lengths first, reserved bytes second, then selected
BLOBs in batches of 64. All phases run on the same prepared-reader connection
and snapshot. Smaller matches keep their prior per-series path.

The read still materializes its answer. The per-query payload, decoded-sample,
block and series limits are unchanged. A missing selected payload yields
`ErrCorrupt` with no partial answer. The wide test verifies exact bits across
sealed blocks and mutable heads, and verifies that an insufficient payload
budget stops before either head or directory BLOB fetches.

## Environment and input

Ryzen 7 7700, Windows 11, Docker Desktop,
Linux 6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs,
Go 1.27.1 linux/amd64, modernc SQLite 1.59.0. The database lived on Docker
volume `tsperf`. Each stage copied the pinned 10,000-series × 500-sample
`arch-s10k/read.db`, SHA-256
`cdf4a070790d6bd98865b68dc5f3ee46f8adae3f5b45a5b2b11835980301405d`.
The compared binaries were built from consecutive uncommitted states of
`codex/architecture-measurements` above `f4568cc`; every comparison used the
same harness and fixtures. One-hour queries returned 360 samples per series.

## Results

Payload batching, compared with batched heads but individual payload SQL:

| selector | old QPS | new QPS | allocs/query, old → new |
|---|---|---|---:|
| 2,500 series, one reader | 11.2; 10.7 | 13.1; 12.8 | ~467,800 → ~345,500 |

The two-run means improved about **18%**. The following group-directory step
was compared with that payload-batched state:

| selector | old QPS | new QPS | allocs/query, old → new |
|---|---|---|---:|
| 2,500 series, one reader | 12.4; 12.5 | 13.4; 13.4 | ~345,400 → ~257,300 |
| 20 series, eight readers | 1,978; 1,994 | 2,132; 2,173 | ~3,273 → ~2,636 |
| 2,500 series, eight readers | 40.8; 40.9 | 40.8; 41.2 | ~345,200 → ~256,900 |

Directory batching improved the one-reader wide QPS about **7%** and the
eight-reader 20-series QPS about **8%** by two-run means. Eight simultaneous
wide reads saturated at roughly 41 QPS in both variants; their throughput
gain is not demonstrated even though allocations fell. The three WP5 steps
together moved the one-reader 2,500-series fixture from **10.4 to 13.4 QPS**,
about **29%**. The old review's 20–28 QPS projection is not supported here.

These measurements report Go allocations and sampled OS RSS, not SQL step
counts or physical read I/O. The new path holds selected compressed bytes
until the snapshot ends and then decodes outside it. Aggregate admission and
streaming remain separate contracts.

## Reproduce and checks

The paired binaries `/perf/perf-arch-headbatch`,
`/perf/perf-arch-payloadbatch` and `/perf/perf-arch-groupbatch` are retained
on `tsperf`; their source states have not been committed. Build and run the
current candidate with:

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-groupbatch .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-groupbatch -dir /perf/arch-s10k -stage read \
  -series 10000 -shape selector_low_cardinality -readers 1 -seconds 6
go test ./metrics -run TestBatchedHeadsPreserveSnapshotAndReserveBytesFirst -count=1 -v
```

`task check` passed on Windows after the batched-head step; the final suite
and Linux race run for the combined path are recorded in
[the worklist](architecture-worklist-2026-09-22.md). Narrow-head decoding,
the redundant value-envelope copy and statement-cache churn remain open.
