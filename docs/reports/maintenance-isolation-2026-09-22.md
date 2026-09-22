# A failing series no longer stops the maintenance pass

This round implements D1 from [the execution review](execution-review-2026-09-22.md)
and the recovery correction in [its verdict](execution-review-verdict-2026-09-22.md).
Per-series `ErrCorrupt` and capacity failures suspend that series, persist a
reason and let the pass continue. SQLite, I/O and context errors still return
from `Maintain`. A suspended series is excluded from both due indexes and
rejects ingestion with `ErrSuspended`.

`Stats.QuarantinedSeries` is the current persisted count, loaded on reopen.
`ListMaintenanceFailures` pages through bounded diagnostics.
`RetryFailedMaintenance` re-enables at most `MaintenanceSeries` failures in one
transaction; damaged data will fail and be suspended again. Retrying after
opening with a raised limit lets the previously oversized head seal. The
schema changes are migrations `0007_maintenance_failures.sql` and
`0008_maintenance_failure_paging.sql`; the latter adds a bounded diagnostic
page scan without rewriting the applied migration.

## Evidence and environment

Windows 11, Ryzen 7 7700, Go 1.27.1 windows/amd64; the race run used
Go 1.27.1 linux/amd64 in Docker Desktop on Windows 11. The fixtures are
created by the named tests in a temporary SQLite file. Each ready series has
241 ordered samples; `MaintenanceSeries=2` for isolation and `=1` for the
bounded paging/retry test. No throughput or storage saving is claimed.

| scenario | previous behavior | current observed behavior |
|---|---|---|
| first ready head has a bad checksum; second is healthy | `Maintain` returns at the first error (D1 code observation) | one series suspended, one block sealed in the same pass |
| open with `MaxHeadSamples=240` after writing a 241-sample head | pass stops at `ErrLimit` | one suspension persists across reopen; retry with limit 512 seals one block |
| two suspended series, retry bound one | no failure lifecycle | one diagnostic per page; one marker cleared per call |

The first column is a code observation from D1, not a timed baseline run.
The current outcomes are assertions in
`TestCorruptSeriesDoesNotStopOtherMaintenance`,
`TestSuspendedLimitCanRecoverAfterReopen` and
`TestMaintenanceFailurePagesAndRetryStayBounded`.

## Reproduce and checks

```sh
go test ./metrics -run 'TestCorruptSeriesDoesNotStopOtherMaintenance|TestSuspendedLimitCanRecoverAfterReopen|TestMaintenanceFailurePagesAndRetryStayBounded' -count=1 -v
go test -shuffle=on ./...
task lint
```

All three targeted tests, the full shuffled suite and lint passed on Windows.
`go test -race ./metrics` passed in the Linux container. The earlier
`task check` attempt could not finish `task size` on this Windows PATH because
`wc` is absent; the independent size binaries were measured in
[the reader-reuse round](reader-reuse-2026-09-22.md).

The remaining operational work from these reviews includes reclaiming empty
series and bounding simultaneous preparation/decode across callers. This
round does not measure how much suspension and retry cost in a large file.
