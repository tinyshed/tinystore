# Reuse completed mutable-head chunks

This round implements the encoding part of A11 in
[the execution review](execution-review-2026-09-22.md). After merging new
samples, ingestion copies each already encoded 240-sample chunk whose end is
strictly before the earliest incoming timestamp. It re-encodes the suffix and
writes a fresh whole-head checksum. A partially filled last chunk is always
re-encoded. The stored format, head budgets, duplicate-last-wins rule and
sealed frontier are unchanged. Decoding the old head is still whole-head work;
the sorted-input preparation path remains open.

`TestUnchangedHeadChunksKeepTheirEncodedBytes` checks the physical prefix and
the bit-exact decoded result for both an append and a late NaN-payload
replacement. The full metrics suite and race run also passed.

## Environment and comparison

Ryzen 7 7700, Windows 11, Docker Desktop, Linux
6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs, Go 1.27.1 linux/amd64,
modernc SQLite 1.59.0. Databases were fresh on the `tsperf` named volume.
Both binaries used the same split harness and the D3 one-UPDATE ingest path.
The old binary was built through a Go overlay of the pre-A11 `head.go` and
`ingest.go`; the new binary used the branch working tree. These source states
are not committed, so this is a provisional branch measurement.

Long head: 100 series seeded with 480 samples each before timing; 100 more
samples per series, 20 series per `Ingest`, no maintenance. Sequential run
order was old, new, new, old:

| | old | new |
|---|---|---|
| samples/s | 2,251; 2,425 | 3,040; 3,194 |
| call p50 | 8.77; 8.13 ms | 6.42; 6.18 ms |
| allocs/sample | 296.8 | 230.2 |
| closed file | 131,072 B | 131,072 B |

The two-run mean throughput improved about **33%** on this long-head workload,
with about **22%** fewer allocations per sample. Observed WAL file growth was
378.63 B/sample in both versions; it is not actual WAL write volume.

Short head: 1,000 series seeded with one sample, then 50 more each, 100 series
per `Ingest`. There was no completed chunk to reuse. Old QPS-equivalent ingest
rates were 6,688 and 7,328 samples/s; new rates were 6,858 and 7,614.
Ranges overlap and allocations were 215.3/sample in both versions, so this
small sample does not establish a short-head speed change.

## Reproduce and checks

The branch's `bench/perf` has `-seed-samples` for an append comparison. Build
the candidate with the current tree and run it on a fresh database:

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-a11-new .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-a11-new -dir /perf/arch-a11-new -stage append \
  -series 100 -seed-samples 480 -samples 100 -batch 20
go test ./metrics -run TestUnchangedHeadChunksKeepTheirEncodedBytes -count=1 -v
```

The pre-change binary `/perf/perf-arch-a11-old` is retained in the same local
volume. It was built from the current harness with `metrics/head.go` from
`f4568cc` and the pre-A11 `s.encodeHead(ctx,id,merged)` call in `ingest.go`.
`go test -shuffle=on ./...`, `task fmt:check`, `task lint` and
`go test -race ./metrics` passed after the change.
