# Reclaim registration after the last sample expires

This addresses WP10 and the audit's `MaxSeries` churn limit. When retention
removes the last head sample and no sealed group remains, the same transaction
deletes the series state, original identity, postings and dictionary pairs
whose posting count reaches zero. It decrements `series_count`, freeing a
configured cardinality slot. A live sibling retains its shared dictionary
pairs. `Maintenance.ReclaimedSeries` counts that pass;
`Stats.ReclaimedSeries` counts committed reclamations by the current handle.

Re-registering identical labels after complete expiration starts a new
lifecycle: the kind and sealed frontier are new. Ingest still refuses any
sample below the current retention cutoff. No user-visible series handle is
exposed, so reuse of a file-local series id does not identify an old lifecycle.
Suspended corrupt series are excluded from maintenance until explicitly retried.

`TestExpiredSeriesReclaimsCardinalityAndAllowsNewLifecycle` exercises a sealed
block plus head under `MaxSeries=1`, replacement by another label set, then
re-registration of the original labels with a new kind across reopen.
`TestReclaimKeepsLabelsUsedByAnotherSeries` checks posting counts and the live
sibling's exact read.

## Environment and file accounting

Ryzen 7 7700, Windows 11, Docker Desktop,
Linux 6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs,
Go 1.27.1 linux/amd64, modernc SQLite 1.59.0. The database was on Docker
volume `tsperf`; the engine was the uncommitted
`codex/architecture-measurements` tree above `f4568cc`.

The file census used `buildSeries(1000)`, one sample per series, a one-second
retention and a two-second wait outside the maintenance timer. A fixed number
of bounded maintenance passes expired all 1,000 samples and reclaimed all
1,000 registrations in 3.857 seconds. This is a single candidate run, not an
old-versus-new throughput comparison. The old audit's probe established that
the same logical empty-series state previously retained its `MaxSeries` slot.

| `dbstat` object | before | after |
|---|---:|---:|
| postings | 81,920 B | 4,096 B |
| series | 73,728 B | 4,096 B |
| series_state | 81,920 B | 4,096 B |
| series_due | 20,480 B | 4,096 B |
| sqlite_autoindex_series_1 | 61,440 B | 4,096 B |

The other reported objects stayed at one 4,096-byte page each. The file
remained **372,736 bytes** (91 pages): the freelist rose from **0 to 73
pages**. Reclamation returns cardinality and reusable SQLite pages; it does
not shrink the file or erase old bytes. The dictionary and its unique index
were already one page each in this short fixture, so deleting their rows did
not reduce their `dbstat` page sizes.

## Reproduce and checks

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-churn .
docker run --rm -v tsperf:/perf golang:1.27 sh -c '
  /perf/perf-arch-churn -dir /perf/arch-churn -stage churn_prepare -series 1000
  /perf/perf-arch-churn -dir /perf/arch-churn -stage objects -file churn.db
  sleep 2
  /perf/perf-arch-churn -dir /perf/arch-churn -stage churn_expire -series 1000
  /perf/perf-arch-churn -dir /perf/arch-churn -stage objects -file churn.db'
go test ./metrics -run 'TestExpiredSeriesReclaimsCardinalityAndAllowsNewLifecycle|TestReclaimKeepsLabelsUsedByAnotherSeries' -count=1 -v
```

The final `task check` and Linux race results are recorded in
[the worklist](architecture-worklist-2026-09-22.md). Long-running churn,
reader/writer contention and page reuse after many generations remain
unmeasured.
