# Shared active-work reservations across Store handles

`metrics.WorkBudget` can now be passed through `Options.SharedBudget` to several
Store handles. A read reserves from its effective query limits before fetching
and holds the reservation through decode. An ingest reserves from the batch
input and head ceiling before preparing, then holds it through commit.
Maintenance reserves from head and staging ceilings. A reservation over capacity
returns `ErrLimit`; a wait is cancellable. Releasing work wakes waiters on all
handles sharing the object. `Usage` reports current and peak reservations.

This bounds the sum of **accounted active-work reservations**, not process RSS.
It does not include Go runtime, SQLite page caches, caller-owned inputs, or a
caller that retains returned results. Read reservations include compressed
payload bytes, decoded and output sample capacities, and per-series overhead;
ingest and maintenance use conservative capacity estimates. Small budgets can
therefore reject operations whose actual allocation would be smaller. Query
limits may be narrowed per request. A separate Store without the same budget
object is outside this control.

## Measurement

Ryzen 7 7700, Windows 11, Docker Desktop Linux amd64, 16 visible CPUs,
Go 1.27.1, modernc SQLite 1.59.0, engine revision `4589144` and harness
revision `dd4a6e5` on `codex/architecture-measurements`.
Fixture: 10,000 series × 500 samples per file, source SHA-256
`cdf4a070790d6bd98865b68dc5f3ee46f8adae3f5b45a5b2b11835980301405d`.
Two Store handles each opened a fresh copy on the `tsperf` volume. Eight workers
issued one-hour region reads returning 2,500 series and 900,000 samples per
query. Each store allowed four active reads. ABBA order was unbounded, shared
240,000,000-byte reservation, same reservation, unbounded; five seconds each.

| Shared budget | QPS, two runs | Peak sampled OS RSS | p99 request latency |
|---|---:|---:|---:|
| none | 51.4; 50.2 | 378.3; 372.7 MiB | 232; 211 ms |
| 240,000,000 B | 22.5; 22.7 | 117.0; 118.3 MiB | 5.23; 4.67 s |

Peak reservation was 231,753,728 bytes, corresponding to two wide reads.
Sampled RSS fell about 69%, while throughput roughly halved and queued tail
latency became seconds. A 20-ms RSS watcher can miss a shorter peak. This is a
resource/latency choice, not a recommendation to make 240 MB the default.
These table values are the post-commit ABBA repetition; the preceding
working-tree run had 50.9–51.0 versus 24.3–24.5 QPS and 375.9–388.5 versus
113.7–114.8 MiB RSS, corroborating the direction but showing timing variance.
`TestSharedWorkBudgetBoundsTwoStoresAndHonorsCancellation` passed on Windows and
with the Linux race detector: a read on one Store held the budget, the second
Store canceled while waiting, then both reads and ingestion succeeded after
release without a leaked reservation. The gate also covers a maintenance wait
on the shared budget, cancellation and successful maintenance after release.

## Reproduce

```sh
<perf> -dir <volume>/arch-s10k -stage multi_store -series 10000 -seconds 5 -shared-bytes 0
<perf> -dir <volume>/arch-s10k -stage multi_store -series 10000 -seconds 5 -shared-bytes 240000000
<perf> -dir <volume>/arch-s10k -stage multi_store -series 10000 -seconds 5 -shared-bytes 240000000
<perf> -dir <volume>/arch-s10k -stage multi_store -series 10000 -seconds 5 -shared-bytes 0
go test -race ./metrics -run '^TestSharedWorkBudgetBoundsTwoStoresAndHonorsCancellation$' -count=1
```

The existing per-Store slots remain active. [Group commit rules](../group-commit-contract.md)
are specified separately; no actor or new checkpoint policy ships in this round.
