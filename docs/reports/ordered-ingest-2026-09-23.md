# Ordered input avoids timestamp maps in ingest preparation

This completes the sorted-input part of WP6. Each series batch first appends
validated samples to an owned slice when timestamps are strictly increasing
across all batches for that identity. At the first duplicate or out-of-order
timestamp it moves the accumulated samples to the previous map-and-sort path.
The final value supplied for a duplicate timestamp still wins. No caller
slice is retained. `TestOrderedPreparationFallsBackForDuplicatesAndLatePoints`
checks the fallback, ownership and exact NaN payload bits.

## Environment and input

Ryzen 7 7700, Windows 11, Docker Desktop,
Linux 6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs,
Go 1.27.1 linux/amd64, modernc SQLite 1.59.0. Fresh databases were on Docker
volume `tsperf`. Old and new binaries shared the same `bench/perf` harness,
schema and every prior engine change on the uncommitted
`codex/architecture-measurements` branch above `f4568cc`.

`append` seeded 1,000 existing series with one sample, then timed 50 new
samples per series in calls of 100 series, without maintenance. `register`
timed one sample in each of 10,000 new series, also 100 series per call. Both
used the deterministic labels, timestamps and values described in
[the instrument round](architecture-instruments-2026-09-22.md). The run order
was old, new, new, old, each against a fresh file:

| stage | old samples/s | new samples/s | allocs/sample, old → new |
|---|---|---|---:|
| append | 8,349; 8,288 | 8,444; 8,675 | 215.27 → 212.27 |
| register | 4,491; 4,458 | 4,666; 4,648 | 301.52 → 298.52 |

The two-run means improved about **3%** for append and **4%** for registration.
The ranges are close enough that this is a modest gain on this machine, not a
new ingest throughput guarantee. Observed WAL file growth and closed-file
size were unchanged in both stage pairs. This optimization removes preparation
work; it does not change durable commit count or writer SQL execution.

## Reproduce and checks

The old binary `/perf/perf-arch-groupbatch` and new binary
`/perf/perf-arch-ordered` are retained on `tsperf`; source states are not yet
committed. Build and run the current candidate with:

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-ordered .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-ordered -dir /perf/arch-ordered-new -stage append \
  -series 1000 -seed-samples 1 -samples 50 -batch 100
go test ./metrics -run TestOrderedPreparationFallsBackForDuplicatesAndLatePoints -count=1 -v
```

The full branch checks are recorded in
[the worklist](architecture-worklist-2026-09-22.md).
