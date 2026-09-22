# Exact posting counts: read gain and registration cost

This implements the cardinality-counter part of WP4 from
[the engine audit](engine-audit-2026-09-22.md) and
[the execution review](execution-review-2026-09-22.md). Migration 0009 adds
`label_values.posting_count` and backfills it from postings. New-series
registration increments all its label counters in the same transaction;
rollback leaves them unchanged. Multi-matcher ranking reads exact counts
instead of probing at most 1,024 posting rows per matcher. A single matcher
resolves only its label id, because it has nothing to rank.

`TestPostingCountsRankAboveTheOldProbeCap` recreates lists of 2,048 and 1,025
entries. The 1,025-entry list now drives the query, and a failed registration
leaves the count at 2,048. Batched matcher-id resolution and the discarded JSON
work in `fillLabels` remain open.

## Environment and inputs

Ryzen 7 7700, Windows 11, Docker Desktop, Linux
6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs, Go 1.27.1 linux/amd64,
modernc SQLite 1.59.0. Databases lived on the `tsperf` named volume. Both
binaries shared the D3 and A11 writer path and the same perf harness. Old and
new binaries were built from consecutive uncommitted working-tree states on
`codex/architecture-measurements` above `f4568cc`.

Read stages used fresh working copies of the pinned 10,000-series × 500-sample
`arch-s10k/read.db`, SHA-256
`cdf4a070790d6bd98865b68dc5f3ee46f8adae3f5b45a5b2b11835980301405d`.
The candidate applied migration 0009 to its stage copy before timing. Each
query returned 360 samples. Registration stages wrote 10,000 fresh series,
one sample each, 100 series per call. Each comparison ran old, new, new, old
sequentially.

## Measurements

| one-hour read | old QPS | new QPS |
|---|---|---|
| one reader, fixed key | 6,738; 6,539 | 8,847; 8,893 |
| one reader, rotating across 10,000 keys | 5,232; 5,199 | 6,503; 6,972 |
| eight readers, fixed key | 7,702; 7,679 | 9,032; 8,745 |
| eight readers, rotating keys | 7,879; 7,661 | 8,639; 8,670 |

The two-run means improved about **34%**, **29%**, **16%** and **11%** in those
four read scenarios, respectively. These are warm three-second stages on one
machine; the earlier reports' QPS numbers used a different harness/schema
state and are not the baseline for this comparison.

| registration | old | new |
|---|---|---|
| samples/s | 4,761; 4,322 | 4,084; 4,010 |
| allocs/sample | 291.8 | 301.5 |
| closed file | 3,153,920 B | 3,158,016 B |

Registration was about **11% slower** by the two-run mean and allocated about
ten more objects per sample. `dbstat` assigns the entire **4,096-byte** file
increase to `label_values`: it grew from 16,384 to 20,480 bytes. `postings`,
`series`, their indexes, `series_state` and the freelist were unchanged for
this fixture. Observed WAL file growth increased from 469.68 to 472.15
B/sample; this is not a count of frames written or device writes.

The read benefit is real for these workloads, but registration pays for it.
The choice remains a workload tradeoff until steady churn and deletion are
measured; future series reclamation must decrement counts transactionally.

## Reproduce and checks

The pre-change binary `/perf/perf-arch-a11-new` and candidate binary
`/perf/perf-arch-postings` are retained in `tsperf`; the pre-change source
state is uncommitted, so this round is provisional. Build the candidate and
run a stage on a copy of the pinned fixture:

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-postings .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-postings -dir /perf/arch-s10k -stage read \
  -series 10000 -shape rotating_hour -readers 8 -seconds 3
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-postings -dir /perf/arch-postings-new -stage register \
  -series 10000 -samples 1 -batch 100
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-objects -dir /perf/arch-postings-new -stage objects -file register.db
```

`task check` passed on Windows, including tests, lint, vulnerability scan and
size; `go test -race ./metrics` passed in the Linux container and
`go -C bench/perf test ./...` passed on Windows. No platform result is inferred
from another operating system.
