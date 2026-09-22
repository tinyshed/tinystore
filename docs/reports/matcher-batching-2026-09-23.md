# Matcher lookup batching needs a threshold

This round tests the remaining matcher-id batching item in WP4. The read path
uses a prepared statement on its owning connection. Replacing every short
lookup with one `json_each(?)` query lost throughput for common two-label
selectors, so the final code retains separate prepared lookups for fewer than
eight matchers. Eight or more matchers use one fixed SQL program and a bound
JSON argument. A missing label still makes the whole match empty, and ranking
still uses the exact posting counts introduced in migration 0009.

The statement is retained by `internal/sqlite`'s 32-program, per-connection
reader cache. Each call still opens a new read snapshot. Writer transactions
do not yet have equivalent statement reuse.

## Environment and inputs

Ryzen 7 7700, Windows 11, Docker Desktop, Linux
6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs, Go 1.27.1 linux/amd64,
modernc SQLite 1.59.0. Read files lived on Docker volume `tsperf`. The source
fixture was the 10,000-series × 500-sample `arch-s10k/read.db`, SHA-256
`cdf4a070790d6bd98865b68dc5f3ee46f8adae3f5b45a5b2b11835980301405d`.
Each timed read stage used a copy. Both variants shared all other changes on
`codex/architecture-measurements` above `f4568cc`; only `rankMatchers` differed.
The old version was built with a Go overlay stored under ignored `bench/corpus/`.

The one-hour reads returned 360 samples. The two-label selector used
`__name__` and `host`; the eight-label selector used all labels on a series.
Each four-second comparison ran old, candidate, candidate, old sequentially:

| matchers | old QPS | all-batched candidate QPS | old → candidate allocs/query |
|---:|---|---|---:|
| 2 | 11,817; 11,550 | 11,069; 11,130 | 467.7 → 439.8 |
| 4 | 10,428; 10,564 | 10,402; 10,223 | 538.9 → 446.9 |
| 8 | 9,400; 9,428 | 9,812; 9,634 | 680.9 → 460.9 |

The two-label path lost about **5%** QPS when batched. Four labels did not
establish a gain. Eight labels saved about 220 allocations per full query,
but its roughly 3% QPS gain varied between repetitions. The final threshold
was therefore measured again against the old path: two-label QPS ranges
overlapped (11,185–11,425 old; 11,245–11,306 hybrid) with the same 467.8
allocations/query. Eight-label runs were 8,959–9,428 old and 9,108–9,270
hybrid, overlapping in QPS but still 680.9 → 460.9 allocations/query.

A separate 32-matcher benchmark isolated `rankMatchers` in a fresh single-series
file. The same benchmark source ran old, hybrid, hybrid, old for three seconds
each, on one read connection per call:

| old | hybrid |
|---|---|
| 89,557; 89,340 ns/op | 52,386; 52,272 ns/op |
| 45,148 B/op; 1,068 allocs/op | 6,421–6,422 B/op; 111 allocs/op |

This is a **41%** reduction in isolated matcher-resolution time. It is not a
41% full-query speedup. The broad case justifies the batch path; the ordinary
short case justifies retaining direct prepared lookups.

## Reproduce and checks

The old overlay maps only `metrics/registry.go` to a copy with per-matcher
prepared lookups. It is under ignored `bench/corpus/matcher-baseline/`; create
it from the pre-batch source to compare a clean checkout. Build both binaries
with identical harness source and alternate runs:

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-matcher-hybrid .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-matcher-hybrid -dir /perf/arch-s10k -stage read \
  -series 10000 -shape all_labels_hour -readers 1 -seconds 4
go test ./metrics -run 'TestMatcherLookupBatchesOnlyLargeSelectors|TestPostingCountsRankAboveTheOldProbeCap' -count=1 -v
```

The Go overlay and paired binaries are not a portable source revision until
the branch work is committed. `task check` and the Linux metrics race run are
reported in [the worklist](architecture-worklist-2026-09-22.md).
