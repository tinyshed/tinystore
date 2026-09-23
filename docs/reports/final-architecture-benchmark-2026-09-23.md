# Saved-revision and three-engine benchmark

This round replaces provisional before/after claims that used transient source
states. It measures saved `f4568cc` (main) and `bfa7232` (architecture branch)
with the same `bench/perf/repro` source copied into detached worktrees. It also
measures the complete TSBS and local Telegraf corpora with TinyStore,
VictoriaMetrics and Prometheus. The later instrumentation, engine API,
representation spike and benchmark harness sources are saved as `aecd7b2`,
`4589144`, `6a0465e` and `dd4a6e5`, respectively. The saved-revision
before/after stage and both public-corpus checks were repeated from saved code.
The [process-RSS round](ram-comparison-2026-09-23.md) separately measures
idle, ingest, light reads, native wide streams and maintenance overlap on all
three engines; its streaming API is in `4589144` and must not be attributed
to the earlier `bfa7232` revision.

## Environment and inputs

AMD Ryzen 7 7700, Windows 11, Docker Desktop 29.6.2, Linux amd64 containers,
16 visible CPUs, Go 1.27.1 linux/amd64, modernc SQLite 1.59.0. Database files
and timed binaries lived on the Docker named volume `tsperf`. All timed stages
ran sequentially. The two corpus inputs were normalized JSONL:

| Input | Series | Samples | SHA-256 |
|---|---:|---:|---|
| TSBS DevOps | 2,020 | 5,090,400 | `e4f502af7b0b2ff2c4dba92057a8f2b95e636882f3cb9900986e013d189572cf` |
| Five-host Telegraf capture | 4,435 | 4,939,641 | `6475b9d3dcf8cf2ce88de5084ea37981d9d5676cdf68086b89115fb1d808ad0d` |

VictoriaMetrics image digest:
`sha256:86ca5fdb6d87d56ba047b044039019ba2bd9042b36e35f6ea34e437b6c825cef`.
It ran with `-precisionBits=64`, `-dedup.minScrapeInterval=0s`, 100-year
retention and 512 MiB allowed memory. Prometheus v3.6.0 image digest:
`sha256:76947e7ef22f8a698fc638f706685909be425dbe09bd7a2cd7aca849f79b5f64`.
Prometheus bulk size is one block made by `promtool tsdb create-blocks-from
openmetrics --max-block-duration=24h`; the real Telegraf OpenMetrics names were
quoted where the text format requires it. VictoriaMetrics size follows import,
force flush, requested asynchronous merge and graceful stop. These are named
storage states, not identical compaction policies.

## Saved-revision throughput

`bench/perf/repro` used identical logical samples and SQL options from its one
source file. Order was base, branch, branch, base; every stage used a new file.
The register stage wrote 1,000 series in ten 100-sample calls. Append seeded
the same series, then wrote 100,000 samples in 1,000 calls. Sealing prepared
64 eligible blocks; read used 1,000 series with 500 samples each and rotating
one-hour windows returning 360 samples.

| Stage | `f4568cc` two runs | `bfa7232` two runs |
|---|---:|---:|
| Register, samples/s | 5,096; 5,161 | 11,369; 12,371 |
| Append, samples/s | 6,258; 6,105 | 13,262; 13,006 |
| Seal, blocks/s | 180.6; 183.6 | 417.1; 417.3 |
| Narrow read, queries/s | 5,881; 5,737 | 11,210; 11,185 |

This supports roughly a doubling on these four shapes. It does not assert the
same gain for broad reads, mixed work or another hardware/storage stack.

The unchanged public corpus integration test ran on each saved revision:

| Corpus | Revision | Ingest + Maintain | Reopened bitwise readback | Closed file |
|---|---|---:|---:|---:|
| TSBS | `f4568cc` | 20.564 s | 1.754 s | 4,546,560 B |
| TSBS | `bfa7232` | 19.577 s | 1.108 s | 4,558,848 B |
| Telegraf | `f4568cc` | 36.563 s | 2.283 s | 5,431,296 B |
| Telegraf | `bfa7232` | 34.644 s | 1.341 s | 5,455,872 B |

The branch spent 12,288 more bytes on TSBS and 24,576 on Telegraf in exchange
for the measured read/write improvements. Both revisions returned every source
sample bit for bit after reopen. The corpora differ in series shape and data;
their timings should not be pooled into one speedup.

## Complete bulk files and exactness

| Corpus | TinyStore `bfa7232` | VictoriaMetrics | Prometheus | VM altered value bits | Prom altered value bits |
|---|---:|---:|---:|---:|---:|
| TSBS | 4,558,848 B | 5,483,781 B | 8,300,769 B | 147,751 | 0 |
| Telegraf | 5,455,872 B | 6,637,929 B | 6,705,378 B | 195,733 | 0 |

Both external engines returned all series and samples in the checked export or
dump; neither had missing or extra points. VictoriaMetrics' `precisionBits=64`
did not preserve all float64 bits. The TinyStore file includes schema, mutable
head, dictionary, indexes and free pages. The Prometheus figure includes its
complete backfilled block; the VictoriaMetrics figure includes all files in
its stopped directory. These files represent different engine lifecycles.

## Live remote-write throughput

`bench/perf/remote` sent the same Prometheus remote-write v1 protobuf/Snappy
requests, one series and at most 1,000 samples per request, from a helper
container on the same Docker network. Its timer excludes JSONL parsing and
protobuf construction, and includes sequential HTTP request/response time.
Prometheus rejected the original historical TSBS timestamps as out of bounds.
For the live test only, the earliest timestamp was mapped to Unix millisecond
`1790119200000`; TSBS offsets were divided by 16 (about 26 minutes total),
and Telegraf offsets kept their original spacing (about 25 minutes). Both
engines received the same transformed points and labels. Bulk file and
exactness results above used the original timestamps.

| Corpus | Requests | VictoriaMetrics samples/s | Prometheus samples/s | VM point-query QPS | Prom point-query QPS |
|---|---:|---:|---:|---:|---:|
| TSBS | 6,060 | 2,493,367 | 2,156,923 | 3,263 | 2,288 |
| Telegraf | 7,336 | 2,137,053 | 1,862,186 | 3,737 | 2,379 |

Each query stage issued 2,000 HTTP instant queries over 128 sampled full-label
selectors and found all 2,000. TinyStore's API is an in-process exact raw range
read and one `Ingest` call is a synchronous SQLite transaction; remote-write
acknowledgements and instant PromQL queries have different costs and durability
semantics. Therefore these external throughput figures are service baselines,
not TinyStore speedup or slowdown ratios. The 128-probe bit check found no
changed TSBS values and 126 changed Telegraf values in VictoriaMetrics; the
complete export counts above are authoritative for exactness.

## Physical write counters and page size

The opt-in `TestPhysicalWriteCounters` disables writer auto-checkpoint on a
fresh file, truncates the WAL before timing each stage, reads modernc's
per-writer `sqlite3_db_status` through `sql.Conn.Raw`, then checks WAL header
frames and a post-stage PASSIVE checkpoint. On Linux, 4 KiB pages gave:

| Stage | Commits | CacheWrite pages | CacheSpill | WAL frames | WAL bytes | Checkpoint log / copied |
|---|---:|---:|---:|---:|---:|---:|
| Register, 1,000 samples | 10 | 285 | 0 | 285 | 1,174,232 | 285 / 285 |
| Append, 10,000 samples | 100 | 380 | 0 | 380 | 1,565,632 | 380 / 380 |
| Seal, 64 blocks | 8 | 62 | 0 | 62 | 255,472 | 62 / 62 |

Linux `strace -f -ttt -yy` around the marked timed sections counted 11, 101
and 9 successful WAL `fsync` calls respectively. That is one per committed
transaction plus one additional WAL sync in each staged section. The trace
reported 569, 745 and 125 WAL `pwrite64` calls; syscall counts are not page
counts. The trace was captured on Docker's Linux volume, not on a Windows
filesystem. Windows and Linux both passed the counter test; the timed values
above are Linux only.

An ABBA 4 KiB/8 KiB page experiment on the same Linux host produced these
deterministic byte counts in both repetitions:

| Stage | 4 KiB WAL / closed file | 8 KiB WAL / closed file |
|---|---:|---:|
| Register | 1,174,232 / 348,160 B | 1,528,208 / 425,984 B |
| Append | 1,565,632 / 364,544 B | 2,218,352 / 458,752 B |
| Seal | 255,472 / 94,208 B | 476,560 / 163,840 B |

The 8 KiB page reduces frames but increases WAL bytes and closed-file bytes
for all three workloads, without a visible elapsed-time benefit in this short
run. Keep 4 KiB as the default. CacheWrite counts pages, not device bytes;
`strace` counts successful system calls, not their persistence latency or the
physical device's write amplification.

## Reproduce and limits

For saved-revision tests, detach worktrees at `f4568cc` and `bfa7232`, copy
the identical `bench/perf/repro` directory into both, build each with its own
`bench/perf/go.mod`, and run each binary sequentially on the same Linux volume:

```sh
<repro-base> -dir <volume>/base-1 -stage append -series 1000 -batch 100 -rounds 100
<repro-head> -dir <volume>/head-1 -stage append -series 1000 -batch 100 -rounds 100
<repro-head> -dir <volume>/head-2 -stage append -series 1000 -batch 100 -rounds 100
<repro-base> -dir <volume>/base-2 -stage append -series 1000 -batch 100 -rounds 100
```

Replace `append` with `register`, `seal` or `read` and the parameters stated
above. The corpus test is `TINYSTORE_JSONL=<corpus> go test ./metrics -run
'^TestCorpusThroughPublicStore$' -v -count=1`. TSBS bulk reproduction is
`./bench/run-tsbs.ps1 -Corpus <corpus>`. For Telegraf, `bench/perf/remote
-stage openmetrics` generates the full OpenMetrics file; run `promtool tsdb
create-blocks-from openmetrics --max-block-duration=24h` and `promtool tsdb
dump` on the resulting block, then `TestEngineExportIsBitExact` with
`TINYSTORE_PROM_DUMP`. Import the same original JSONL to VictoriaMetrics,
force flush, export the complete time range, request merge and stop gracefully;
run the same test with `TINYSTORE_ENGINE_EXPORT`. The test logs differences
rather than failing on them, so inspect its matched, missing and wrong-bit
counters. For physical writes, run `TINYSTORE_PHYSICAL=1
TINYSTORE_PAGE_SIZE=4096 go test ./metrics -run
'^TestPhysicalWriteCounters$' -v -count=1`, then repeat with 8192. The
`strace` command must include `write,fsync,fdatasync,pwrite64` and filter lines
between the emitted `PHYSICAL_BEGIN`/`PHYSICAL_END` markers.

These runs did not verify native macOS runtime, multi-day live operation,
power-loss recovery, or equal durability semantics across engines. Linux race
tests and Windows `task check` passed; native macOS CI could not run because
the GitHub Actions quota was exhausted and the user waived that run. No result
from them licenses an approximate answer from TinyStore.
