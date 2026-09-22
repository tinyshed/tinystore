# The work hidden behind a read

This follows [the selectivity round](selectivity-2026-09-22.md). The next useful
step was a CPU profile, not another reader-pool increase. Preparing SQL consumed
38.1% of sampled CPU in an eight-reader hour query. Caching statements alone
left 30.1% there: SQLite recompiled them when the bound `LIMIT ?` changed.
After both changes below, preparation accounted for 0.57% of sampled CPU.

## Environment and comparison

Ryzen 7 7700, Docker Desktop on Windows 11, Linux
6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs, `golang:1.27`, Go 1.27.1
linux/amd64, modernc SQLite 1.59.0, klauspost/compress 1.19.0. WAL and
`synchronous=FULL` are unchanged. Files live on the `tsperf` Docker volume.

Baseline: `2b3f169`. Candidate: `44ee471`, measured before its commit on that base. Both binaries ran sequentially in the same session, against the
same existing files from `bench/perf/run.sh populate`. Each table entry is a
three-second run without profiling. Separate six-second runs supplied CPU
profiles; their throughput is not used in the table. These are warm reads.

Fixtures: 1,000 series × 2,000 samples; 10,000 × 500; 100,000 × 50. The harness
generates a random integer walk, so a fresh population is a new fixture. The
closed input files used here have these SHA-256 hashes, in that order:

```text
44704d1ab5a2e2924bf828cbdf11ccff5b63750f9e5a3a6ba8b54157fc745fa3
4d41358bd6c0854baffba05829406090559237803390bd8e12eac4c2da44c436
2a620fa4f69db4154e51c73952ad0777850869178dc2f398b2460794c5fbf13b
```

## What changed

Each reader connection retains at most 32 prepared statements, evicting the
oldest on overflow. It keeps SQL programs, never query results. Every callback
still begins and ends a read transaction on that same connection; cancellation
discards the connection and its statements. The writer is unchanged.

Two details defeated the first attempts. Go's `Tx.StmtContext` prepares a
connection-owned statement again, so it cannot preserve this cache. And SQLite's
integer-expression planning marks a bare parameter used as a limit for
repreparation after binding. `LIMIT CAST(? AS INTEGER)` keeps the checked integer
budget while avoiding that parameter-dependent planning. The unchanged index
predicates still select the rows; no range or sample budget was relaxed.

## Results

| query and file | readers | qps before | qps after | p50 before → after | p99 before → after |
|---|---:|---:|---:|---:|---:|
| hour, 10k series, 360 samples | 1 | 3,209.5 | **8,300.0** | 278.3 → 92.3 µs | 752.7 → 470.8 µs |
| same | 2 | 5,530.5 | **10,172.0** | 299.9 → 136.2 µs | 934.3 → 656.3 µs |
| same | 4 | 5,935.0 | **9,484.2** | 673.5 → 427.6 µs | 1,333.4 → 993.6 µs |
| same | 8 | 5,943.8 | **9,017.2** | 1,308.9 → 811.5 µs | 2,467.0 → 2,036.5 µs |
| region/hour, 10k series, 900k samples | 1 | 4.9 | **10.2** | 202.2 → 98.3 ms | 214.1 → 103.2 ms |
| same | 8 | 15.9 | **26.1** | 508.8 → 315.5 ms | 568.5 → 384.4 ms |
| point, 100k series, one sample | 1 | 3,323.9 | **8,331.2** | 276.0 → 103.5 µs | 595.8 → 374.6 µs |
| full scan, 1k series, 2m samples | 1 | 5.4 | **8.1** | 183.7 → 121.9 ms | 195.0 → 134.4 ms |

The broad-query result explains a gap in the preceding report: host, region and
scan shapes each have **one** matcher. Their overhead cannot be attributed to
having more matchers. Repeated SQL preparation per matched series was substantial.

This does not establish linear reader scaling. The light query still saturates,
now around two readers, and loses throughput at higher concurrency. The removed
work made one reader 2.59× faster and eight readers 1.52× faster; the remaining
scheduling and allocation costs have not been isolated here.

There is a cost: the hour query at one reader rose from 447.7 to 482.0 Go
allocations per query. The region query at eight readers peaked at 297.4 versus
311.3 MiB heap. A three-second heavy run has few observations, so its p99 and heap
are descriptive, not stable capacity guarantees. No mixed workload, cold cache,
storage-density or ingest improvement is claimed.

## Reproduce

Build both revisions into the same volume; the baseline source stays separate:

```powershell
docker run --rm -v <repo>:/src -v tsperf:/perf `
  -v tinystore-gocache:/go -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache `
  -e GOWORK=off -w /src/bench/perf golang:1.27 sh -c '
    mkdir -p /perf/baseline-src
    git -C /src -c safe.directory=/src archive 2b3f169 | tar -x -C /perf/baseline-src
    cd /perf/baseline-src/bench/perf
    go build -o /perf/perf-before .
    cd /src/bench/perf
    go build -o /perf/perf-after .'
```

Use existing populated files, or build `/perf/perf` and run `sh run.sh populate`
as in the preceding report. Do not populate between the two variants.

```sh
for r in 1 2 4 8; do
  for v in before after; do
    /perf/perf-$v -dir /perf/s10k -stage read -label "$v" \
      -series 10000 -shape hour -readers "$r" -seconds 3
  done
done
```

Run that loop inside the same container. For the region query use
`-shape selector_low_cardinality` and readers 1 and 8; for the point use
`-dir /perf/s100k -series 100000 -shape point -readers 1`; for the full scan use
`-dir /perf/s1k -series 1000 -shape scan_all -readers 1`.

The harness now accepts `-cpu-profile /perf/read.cpu` and
`-mutex-profile /perf/read.mutex`. Use a separate run for profiling:

```sh
/perf/perf-after -dir /perf/s10k -stage read -series 10000 \
  -shape hour -readers 8 -seconds 6 -cpu-profile /perf/read.cpu
go tool pprof -top -cum /perf/perf-after /perf/read.cpu
```

## Checks

Windows: formatting, lint, module tidiness, `go test -shuffle=on ./...`,
govulncheck and the benchmark module's `go test ./...` passed. `task check`
reached the final size-report command and failed because `wc` is absent;
both Linux probe binaries had built successfully. Their sizes were read with
PowerShell instead.

Linux container: `CGO_ENABLED=0 go test -shuffle=on ./...` and
`go test -race ./internal/sqlite ./metrics` passed. New tests cover snapshot
consistency across a concurrent write, fresh results on the next snapshot,
statement reuse and eviction, rebinding, read-only enforcement and recovery
after cancellation. Existing engine budget, corruption and concurrent-reader
tests remain in place.
