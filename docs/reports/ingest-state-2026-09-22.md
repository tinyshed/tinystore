# One mutable-state read and one ingest UPDATE

This experiment implements D3 from
[the execution review](execution-review-2026-09-22.md), before writer statement
reuse or chunk reuse. The common packed-head path now reads its state and tail
once, decodes and merges, then makes one UPDATE. It assigns `ready` and
`next_gc_ts` only if their values change, and deletes legacy head rows only
when the old head came from that table. Existing frontier, version, capacity
and exact-value checks remain.

`TestExistingSeriesIngestAvoidsUnchangedDueIndexes` compiles the production
UPDATE through SQLite `EXPLAIN`: an append with unchanged due fields has **0**
`IdxDelete`/`IdxInsert` opcodes; crossing the 240-sample ready threshold has
**2**. This is compiled-program evidence, not a physical page-write count.

## Environment and input

Ryzen 7 7700, Windows 11, Docker Desktop, Linux
6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs, Go 1.27.1 linux/amd64,
modernc SQLite 1.59.0. Fresh databases were created on the `tsperf` Docker
volume for each stage. The pre-change and post-change binaries came from the
working tree on `codex/architecture-measurements` above `f4568cc`; the source
diff was not committed. The existing first-stage reader and maintenance changes
were present in both binaries. The split harness and options were identical.

`append`: 1,000 registered series seeded with one sample outside timing, then
100 rounds of one sample per series, 100 series per `Ingest`, no sealing.
`register`: 10,000 new series with one sample each, 100 series per `Ingest`.
Labels come from `buildSeries`; timestamps use a common 10-second step and
values `(round*7+series)%97`. Runs were sequential old, new, new, old.

| stage | old samples/s | new samples/s | old / new allocs per sample | closed file |
|---|---|---|---|---:|
| append | 5,382; 5,282 | 6,757; 6,190 | 248.0 / 218.8 | 413,696 B both |
| register | 3,887; 3,880 | 4,152; 4,146 | 318.8 / 290.8 | 3,153,920 B both |

On this append workload the two-run mean improved about **21%** and allocated
about **12%** fewer objects per sample. Registration improved about **7%**.
The sampled WAL file-growth values were 26.49 versus 26.41 B/sample for append,
and 469.68 B/sample for both registration binaries. They do not measure WAL
frames written, syncs or device write amplification. The equal closed-file
sizes show no storage-density change for these fixtures.

## Reproduce and checks

The two binaries were built with the same `go build` command at the states
before and after this change, as `/perf/perf-arch-split` and
`/perf/perf-arch-d3`. The pre-change binary is retained in `tsperf` for direct
comparison; a clean source revision for it is not yet committed.

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-d3 .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-d3 -dir /perf/arch-d3-new -stage append \
  -series 1000 -samples 100 -batch 100
go test ./metrics -run TestExistingSeriesIngestAvoidsUnchangedDueIndexes -count=1 -v
```

The full shuffled root suite, formatting and lint passed on Windows. The
remaining writer proposals are bounded statement reuse, encoded-chunk reuse,
sorted-input preparation and a separately measured maintenance publication
batch. Those effects must be compared against this changed baseline.
