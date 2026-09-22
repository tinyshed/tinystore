# Ready means a whole sealable prefix

This completes the `ready`/`clearReady` churn item in WP2 from
[the engine audit](engine-audit-2026-09-22.md). Ingest now sets `ready` only
when at least 240 retained head samples are strictly before that series'
watermark. Publication and expiry recompute the same predicate for the
remaining head. The watermark still follows `max_seen_ts`, and the frontier
still moves only in the publication transaction.

`TestReadyWaitsForASealableWatermarkPrefix` checks 240 points inside a
one-hour lateness window, twenty further appends and maintenance calls, then
a later sample that makes a 240-point prefix safe. Ready stays zero while
unsafe and becomes one before the safe block seals.

## Environment and measurement

Ryzen 7 7700, Windows 11, Docker Desktop, Linux
6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs, Go 1.27.1 linux/amd64,
modernc SQLite 1.59.0, database on the `tsperf` named volume. Both binaries
used the same current harness and other engine changes. The old binary was
built through a Go overlay restoring the previous count-only ready rule;
the new binary used the working tree. Both states are uncommitted, so this
is a provisional branch comparison.

The stage writes 240 initial points, then repeats one append and one
`Maintain` 100 times, with `Lateness=1h` and 10-second intervals. No full
prefix is eligible and no block is sealed. Runs were old, new, new, old:

| | old | new |
|---|---|---|
| Maintain p50 | 3,680; 3,704 µs | 158; 163 µs |
| whole stage | 0.75; 0.76 s | 0.36; 0.37 s |
| observed WAL file growth | 1.61; 1.63 MB | 0.40; 0.41 MB |
| closed file | 73,728 B | 73,728 B |

The no-work maintenance call is about **23 times shorter** in this scenario;
the whole ingest-plus-maintenance loop is about twice as fast. The WAL figure
is sampled file-length growth, not frames written or device I/O. This change
does not claim a general ingest or sealing throughput improvement.

## Reproduce and checks

The binaries `/perf/perf-arch-ready-old` and `/perf/perf-arch-ready-new`
are retained on `tsperf`. Build the current one and run a fresh stage:

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-ready-new .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-ready-new -dir /perf/arch-ready-new \
  -stage ready_churn -samples 100
go test ./metrics -run TestReadyWaitsForASealableWatermarkPrefix -count=1 -v
```

The final `task check`, benchmark-module build and metrics race run are
recorded in [the worklist](architecture-worklist-2026-09-22.md).
