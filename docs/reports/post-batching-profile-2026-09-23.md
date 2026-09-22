# Read profile after batching and prepared writes

This is a fresh prioritization probe for WP5/D7 and WP9 after the read-path
batching rounds. It changes no production behavior. The old execution review's
expected-effects table is not carried forward as a throughput forecast.

## Environment and fixture

Ryzen 7 7700, Windows 11, Docker Desktop,
Linux 6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs,
Go 1.27.1 linux/amd64, modernc SQLite 1.59.0. The engine revision was
`7bcc8ec` with harness `64ce638`, on `codex/architecture-measurements`.
Each read stage copied the 10,000-series × 500-sample
`arch-s10k/read.db`, SHA-256
`cdf4a070790d6bd98865b68dc5f3ee46f8adae3f5b45a5b2b11835980301405d`.
The database lived on Docker volume `tsperf`. CPU profiles ran for eight
seconds with Go's mutex profiler enabled for the eight-reader stage. Profiling
changes throughput, so the stage QPS is not compared with unprofiled rounds.

## Findings

Eight concurrent one-series, one-hour reads sampled 36.95 CPU seconds over
8.04 wall seconds. Cumulative CPU stacks included `fetchSnapshot` at 48.9%,
`matchSeries` at 28.7%, `decodeResults` at 11.6%, and `readOrdinary` at 6.4%.
The mutex profile sampled 20.62 seconds of contended delay; database/sql
`DB.retry` appeared on 23.6% of that delay, while `Codec.Decode` appeared on
3.1%. These stacks overlap and are not additive or a bound on achievable QPS.

One wide region read returned 2,500 series × 360 samples per call. Its CPU
profile sampled 10.69 seconds over 8.04 wall seconds: `decodeResults` was
50.2% cumulative, `readOrdinary` 29.2%, and `fetchSnapshot` 29.1%.
Within `readOrdinary`, the codec `Decode` call accounted for 0.56 sampled
seconds and `Iterator.Next` for 1.99 cumulative seconds. The source lines
constructing the temporary ordinary-value envelope and checksum carried only
small samples, each at most 0.04 seconds. This is evidence that the whole
ordinary-value path matters on wide reads, not evidence that removing the
envelope alone would recover its 29.2% cumulative share.

The next D7 step should isolate the envelope copy/second checksum in a
benchmark and retain bounded structure validation. A general public decoder
that accepts unchecked value streams is not justified by this profile.
WP9's `sql.Conn.Raw` execution-layer comparison remains a separate benchmark
with cancellation and connection-reuse gates; this profile alone does not
authorize direct lib bindings or weakened statement deadlines.

## Reproduce

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-current .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-current -dir /perf/arch-s10k -stage read \
  -series 10000 -shape hour -readers 8 -seconds 8 \
  -cpu-profile /perf/arch-current-read.cpu \
  -mutex-profile /perf/arch-current-read.mutex
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-current -dir /perf/arch-s10k -stage read \
  -series 10000 -shape selector_low_cardinality -readers 1 -seconds 8 \
  -cpu-profile /perf/arch-current-wide.cpu
docker run --rm -v tsperf:/perf golang:1.27 \
  go tool pprof -top -cum -nodecount=55 \
  /perf/perf-arch-current /perf/arch-current-wide.cpu
```

The root `task check` and Linux metrics race run had already passed for
`7bcc8ec`; this round ran the named profiles and `pprof` inspections only.
