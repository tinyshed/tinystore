# Corrected performance labels and separated work shapes

This is a partial WP1 round from [the execution review](execution-review-2026-09-22.md)
and [the engine audit](engine-audit-2026-09-22.md). The harness now calls sampled
WAL file growth what it measures, reports Go `Sys` separately from OS RSS, and
counts generated samples in linear time. New stages separate first-time
registration from repeated append to existing series. `rotating_hour` selects
across the whole series set with matchers built before timing.

Actual WAL frames written, sync count, SQLite cache writes/spills and fresh
per-reader-count mixed fixtures are still unmeasured. None of the growth values
below is a write-amplification measurement.

## Environment and fixtures

Ryzen 7 7700, Windows 11, Docker Desktop, Linux
6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs, Go 1.27.1 linux/amd64,
modernc SQLite 1.59.0. Databases lived on the `tsperf` Docker volume. Engine
source was the uncommitted working tree on `codex/architecture-measurements`
above `f4568cc`; earlier reader and maintenance fixes were already present.

The read fixture was 10,000 series × 500 samples. Opening the earlier `s10k`
file applied schema migration 0007 before these runs, so its `read.db` SHA-256
was `cdf4a070790d6bd98865b68dc5f3ee46f8adae3f5b45a5b2b11835980301405d`.
A copy with those bytes is pinned in the volume at `/perf/arch-s10k/read.db`.
The earlier `4d41358b…` file is no longer present in the volume; these numbers
must not be attributed to its byte-for-byte fixture.
The harness now copies `read.db` before each read or mixed stage, so later
schema upgrades and mixed writes affect only the stage copy.
Each query returned 360 samples from a randomized one-hour window. The write
fixtures used `buildSeries(1000)`, a common 10-second timestamp step and the
deterministic value `(round*7+series)%97`. `register` wrote one sample per new
series; `append` seeded the same series outside timing, then timed 100 new
samples per series, 100 series per `Ingest` call. No sealing ran in append.

## Measurements

Two sequential runs of each write stage, each on a fresh file:

| stage | samples | samples/s | Ingest p50 | sampled WAL growth/sample | peak OS RSS |
|---|---:|---:|---:|---:|---:|
| register 1 | 1,000 | 5,175 | 19.0 ms | 1,219.52 B | 18.4 MiB |
| append 1 | 100,000 | 6,675 | 14.9 ms | 26.62 B | 23.2 MiB |
| register 2 | 1,000 | 5,128 | 19.0 ms | 1,219.52 B | 18.5 MiB |
| append 2 | 100,000 | 6,186 | 15.7 ms | 26.62 B | 22.9 MiB |

The stages have different series histories and durations; their throughput
numbers are baselines for later changes to the same stage, not a speedup ratio.
The WAL column is positive changes in observed file length. SQLite can reuse
WAL space without growing the file, so the column cannot estimate bytes written.

Fixed-key and rotating-key reads ran in the order fixed, rotating, rotating,
fixed, with three seconds per row and no concurrent stages:

| readers | fixed-key QPS | rotating-key QPS | fixed / rotating peak OS RSS |
|---:|---|---|---|
| 1 | 8,354; 8,380 | 6,700; 7,025 | 21.4–21.5 / 23.2–23.5 MiB |
| 8 | 9,492; 9,353 | 10,142; 10,094 | 24.9–25.0 / 43.2–43.3 MiB |

The working set changes both performance and memory, and its direction differs
by reader count here. These data do not identify the contended component. The
RSS values are 20-ms samples of `/proc/self/statm`; a short peak between samples
may be missed. One separate one-reader check reported 24.3 MiB OS RSS while Go
`Sys` was 19.4 MiB, confirming that the old `peak_process_mib` label was false.

## Reproduce and checks

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-split .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-split -dir /perf/arch-write-split -stage register \
  -series 1000 -samples 100 -batch 100
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-split -dir /perf/arch-write-split -stage append \
  -series 1000 -samples 100 -batch 100
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-split -dir /perf/arch-s10k -stage read -series 10000 \
  -shape rotating_hour -readers 8 -seconds 3
```

`go -C bench/perf test ./...` and the Linux build passed. The root test, lint
and race checks for the maintenance change are recorded in
[its round](maintenance-isolation-2026-09-22.md). This harness round does not
change the root module graph.
