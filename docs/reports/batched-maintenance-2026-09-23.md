# Byte-bounded maintenance publication with savepoints

This implements WP7 from [the execution review](execution-review-2026-09-22.md).
`Maintain` still reads and encodes outside the writer. It stages at most eight
ready series and at most 1 MiB of accounted points, clocks and bodies, then
publishes them in one immediate transaction. Each series has a savepoint:
`ErrConflict` rolls back that series, and local corruption suspends it while
other staged series can commit. A SQLite, I/O or context error aborts the
whole transaction. `SealedBlocks`, conflicts and suspension counters are
reported only after the outer commit succeeds. Retention and short-head
`clearReady` work remain separate transactions.

`TestPublicationBatchRollsBackOneConflictingSeries` checks that one changed
candidate leaves its head intact while its neighbor publishes. A trigger
induced write failure in `TestPublicationBatchDoesNotCountRolledBackTransaction`
proves that a previously staged publication is rolled back and counted as zero.
The original single-publication rollback and snapshot tests still pass.

## Environment and input

Ryzen 7 7700, Windows 11, Docker Desktop,
Linux 6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs,
Go 1.27.1 linux/amd64, modernc SQLite 1.59.0, database on Docker volume
`tsperf`. Both binaries shared the prepared-writer work from
[the previous round](prepared-writes-2026-09-23.md) and the same benchmark
harness. Source states are consecutive uncommitted working-tree versions on
`codex/architecture-measurements` above `f4568cc`.

The stage creates 64 fresh series with 481 samples each before timing. With
zero lateness, one `Maintain` publishes two new 240-sample blocks per series,
128 new blocks in all. Four sequential fresh-file runs were old, new, new,
old:

| | old individual commits | new bounded batches |
|---|---:|---:|
| `Maintain` elapsed | 328.2; 346.4 ms | 140.0; 139.5 ms |
| newly sealed blocks | 128 both | 128 both |
| closed file | 114,688 B both | 114,688 B both |
| observed WAL file growth | 1.87; 1.76 MB | 0.24; 0.24 MB |

The two-run mean latency improved about **59%**. With eight staged series per
transaction, this workload uses eight publication transactions instead of
64. That is a code-level count, not a measured fsync count. The sampled WAL
file-length difference is not actual WAL bytes written. The same closed-file
size and block count show this change affects execution, not density.

## Reproduce and checks

The old and new binaries `/perf/perf-arch-maint-old` and
`/perf/perf-arch-maint-new` are retained on `tsperf`; their source states have
not been committed. Build and run the current candidate with:

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-maint-new .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-maint-new -dir /perf/arch-maint-new \
  -stage maintenance_batch -series 64 -samples 481
go test ./metrics -run 'TestPublicationBatch' -count=1 -v
```

`task check` passed on Windows; `go test -race ./internal/sqlite ./metrics`
passed in the Linux container; `go -C bench/perf test ./...` passed on Windows.
Group commit of independent user `Ingest` calls remains a separate, much
larger API and cancellation decision.
