# Validation without discarded identity JSON on reads

This is the label-work part of WP4 from
[the engine audit](engine-audit-2026-09-22.md). Validation and canonical ordering
now have their own function. Ingest still serializes the exact label identity;
`Read`, dictionary reconstruction and legacy-label validation no longer
marshal JSON that they immediately discard. The labels, collision check and
query result are unchanged.

## Environment and input

Ryzen 7 7700, Windows 11, Docker Desktop, Linux
6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs, Go 1.27.1 linux/amd64,
modernc SQLite 1.59.0. Both binaries included the exact posting counts from
[the prior round](posting-counts-2026-09-22.md); only label validation changed.
They were built from consecutive uncommitted states of
`codex/architecture-measurements` above `f4568cc`.

The harness copied the pinned 10,000-series × 500-sample fixture before each
stage. Source SHA-256:
`cdf4a070790d6bd98865b68dc5f3ee46f8adae3f5b45a5b2b11835980301405d`.
The one-hour fixed and rotating queries returned 360 samples each. The wide
region query returned 2,500 series × 360 samples. Stages ran sequentially.

| read shape | old QPS | new QPS | allocs/query, old → new |
|---|---|---|---|
| one reader, fixed key, 3 s each | 8,640; 8,911 | 9,633; 9,512 | 477.8 → 467.7 |
| eight readers, fixed key, 3 s each | 8,175; 8,418 | 8,642; 8,764 | 482.4 → 469.0 |
| one reader, rotating keys, 5 s each | 6,965; 6,682 | 7,106; 7,002 | 486.2 → 475.9 |
| one reader, 2,500-series region, 5 s each | 8.5; 8.1 | 8.2; 8.3 | about 567,800 → 555,400 |

The fixed-key means improved about **9%** with one reader and **5%** with
eight. The longer rotating-key comparison showed about **3%** by its two-run
mean, but shorter runs overlapped and this is a small effect. The wide result
has no demonstrated throughput change despite about 12,400 fewer allocations
per query. No on-disk bytes changed.

## Reproduce and checks

The baseline binary `/perf/perf-arch-postings` and candidate
`/perf/perf-arch-nojson` are retained on `tsperf`; the pre-change source state
was not committed, so the comparison is provisional. Build the candidate and
run an individual shape:

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-nojson .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-nojson -dir /perf/arch-s10k -stage read \
  -series 10000 -shape hour -readers 8 -seconds 3
```

The full root test suite passed after the change; the final `task check`
and race check are recorded with the branch worklist. Matcher-id batching and
bounded batch reads remain separate experiments.
