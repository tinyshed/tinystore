# Commit-grouping economics before an actor

`Ingest` already accepts multiple series in one atomic call. This round prices
the successful-path ceiling available to any future group-commit actor by
changing only how many independent one-sample series requests are placed in
one call. It does not implement an actor, per-request savepoints, queueing or
the [cancellation rules](../group-commit-contract.md).

## Environment and workloads

Ryzen 7 7700, Windows 11, Docker Desktop 29.6.2 Linux amd64, 16 visible CPUs,
Go 1.27.1, modernc SQLite 1.59.0, 4 KiB pages, WAL `synchronous=FULL`, data
on named volume `tsperf`. The throughput harness `bench/perf/repro` was built
against saved revision `bfa7232` and ran fresh files in order 1, 10, 100,
100, 10, 1. Each stage registers the same 1,000 series with one deterministic
sample per series. The physical writer test is saved in `aecd7b2` and engine
source in `4589144`; it used its own fixed 1,000-series fixture, one fresh file per
group size, auto-checkpoint disabled and a truncated WAL before timing. Do
not compare closed-file byte counts between these two fixtures.

| Samples per `Ingest` | Calls / commits | Saved-revision throughput | Call p50 | Physical WAL frames / bytes | Successful timed WAL `fsync` |
|---:|---:|---:|---:|---:|---:|
| 1 | 1,000 | 340; 333 samples/s | 3.22; 3.28 ms | 10,027 / 41,311,272 B | 1,001 |
| 10 | 100 | 2,440; 2,618 samples/s | 3.98; 3.70 ms | 1,678 / 6,913,392 B | 101 |
| 100 | 10 | 11,516; 11,894 samples/s | 8.28; 8.24 ms | 285 / 1,174,232 B | 11 |

`CacheWrite` matched each physical WAL frame count, `CacheSpill` was zero, and
the post-stage PASSIVE checkpoint reported every frame copied. The extra
`fsync` above the commit count occurred once in each marked stage; the trace
does not attribute its duration or storage-device write amplification. The
physical test's closed file was 348,160 bytes at all three group sizes.
Tracer overhead changes elapsed time, so throughput figures come from the
untraced saved-revision harness.

Grouping ten successful samples per commit produced about 7–8 times the
single-request throughput; grouping one hundred produced about 34–36 times.
This is an upper bound on what an actor might recover from callers that cannot
batch themselves. An actor adds queue delay, per-request savepoints and error
handling and must preserve per-call atomicity. It cannot acknowledge any
request before the common durable COMMIT, and a COMMIT failure must notify all
waiters of the uncertain outcome. No actor is shipped on these measurements.
Explicit caller batching remains the implementation choice until a workload
requiring independent low-volume calls justifies the extra machinery.

## Reproduce

```sh
<repro-bfa7232> -dir <new-volume-directory> -stage register -series 1000 -batch 1
<repro-bfa7232> -dir <new-volume-directory> -stage register -series 1000 -batch 10
<repro-bfa7232> -dir <new-volume-directory> -stage register -series 1000 -batch 100
TINYSTORE_PHYSICAL=1 TINYSTORE_REGISTER_BATCH=1 go test ./metrics -run '^TestPhysicalWriteCounters/register$' -v -count=1
TINYSTORE_PHYSICAL=1 TINYSTORE_REGISTER_BATCH=10 go test ./metrics -run '^TestPhysicalWriteCounters/register$' -v -count=1
TINYSTORE_PHYSICAL=1 TINYSTORE_REGISTER_BATCH=100 go test ./metrics -run '^TestPhysicalWriteCounters/register$' -v -count=1
```

Repeat the throughput commands in reverse order on new directories. On Linux,
run the compiled physical test under `strace -f -ttt -yy -e
trace=write,fsync,fdatasync`, then count successful WAL `fsync` calls between
`PHYSICAL_BEGIN register` and `PHYSICAL_END register`. The timing and physical
fixtures are intentionally separate so syscall tracing cannot skew throughput.
