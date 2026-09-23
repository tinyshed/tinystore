# Process RSS across idle, ingest, reads and maintenance

This round answers the seven RAM shapes requested for TinyStore, VictoriaMetrics
and Prometheus on the same 2,020-series, 5,090,400-sample TSBS DevOps corpus.
It measures peak sampled **process RSS**, not Go heap, virtual memory, container
memory, page cache, or an enforceable memory ceiling. The TinyStore API source
is saved as `4589144` on `codex/architecture-measurements`, and the benchmark
harness as `dd4a6e5`. The initial passes used the same code before those commits;
a full TinyStore pass and extra light-read repetitions were rerun from the
saved binaries.

## Environment and fixture

Ryzen 7 7700, Windows 11, Docker Desktop 29.6.2 with Linux amd64 containers,
16 visible CPUs, Go 1.27.1, modernc SQLite 1.59.0, database files on the Docker
named volume `tsperf`. TSBS normalized JSONL SHA-256:
`e4f502af7b0b2ff2c4dba92057a8f2b95e636882f3cb9900986e013d189572cf`.
TinyStore read runs use the full corpus file built and checked by
`TestCorpusThroughPublicStore`. VictoriaMetrics uses pinned image digest
`sha256:86ca5fdb6d87d56ba047b044039019ba2bd9042b36e35f6ea34e437b6c825cef`
with `-precisionBits=64`, zero dedup interval and 512 MiB allowed memory.
Prometheus v3.6.0 uses image digest
`sha256:76947e7ef22f8a698fc638f706685909be425dbe09bd7a2cd7aca849f79b5f64`,
an empty scrape configuration and its remote-write receiver. Each loaded service
run restarts the server process before the timed shape. The reader process is
separate from the measured server process.

The live services received the same samples with the TSBS clock compressed by
16 and shifted to start at Unix millisecond `1790119200000`; this kept the
entire seven-hour source series in a recent 26-minute interval accepted by
Prometheus. TinyStore reads retain the source clock. For each read, the sample
sequence and label selection are the same:

- **Light:** `cpu_usage_guest{hostname="host_0"}`, first 360 samples, one or
  eight concurrent workers for five seconds.
- **Wide:** `{region="eu-west-1"}`, all 303 matching series and 763,560
  samples, one or two concurrent workers for five seconds.

TinyStore's light call is `Read`. Its wide call is `Stream`, which fetches one
SQLite snapshot, releases the read transaction, then passes one owned series
at a time to a callback. The callback only counts samples. VictoriaMetrics'
wide call is [streaming `/api/v1/export`](https://docs.victoriametrics.com/victoriametrics/)
with `reduce_mem_usage=1` and `max_rows_per_line=1000`; the client verifies
763,560 JSON values on every call. Prometheus' wide call is the
[streamed remote-read API](https://prometheus.io/docs/prometheus/latest/querying/remote_read_api/)
with `STREAMED_XOR_CHUNKS`. The client verifies CRC32C on every response frame
and counts 763,560 samples from the chunk headers on every call. The two
protocols differ in bytes, parsing and server work; these are each engine's
native streaming paths, not one identical wire API.

TinyStore RSS is sampled every 20 ms from `/proc/self/statm`; the server RSS is
sampled on the same interval by a sidecar in its PID namespace reading
`/proc/1/status` (`VmRSS`). A brief peak between samples can be missed. Each
read run uses a fresh process or restarts the loaded service. Ingest and
maintenance use fresh database files. The table reports all observed peaks
from at least two passes. Wide streaming has four external runs in ABBA order;
TinyStore's light eight-reader row has extra saved-revision repetitions.

## Peak process RSS

| Shape | TinyStore MiB | VictoriaMetrics MiB | Prometheus MiB |
|---|---:|---:|---:|
| Idle, opened file, five seconds | 12.2–12.3 | 44.0–58.1 | 92.0–93.8 |
| 1 light read | 20.9–21.2 | 51.3 | 103.6–104.9 |
| 8 light reads | 25.5–31.9 | 62.5–63.1 | 122.7–125.3 |
| 1 native wide streamed read | 26.9–27.6 | 103.4–106.5 | 99.6–99.7 |
| 2 native wide streamed reads | 36.4–37.2 | 139.3–144.7 | 101.7–102.7 |
| Ingest complete corpus | 29.1–29.3 | 288.4–316.9 | 136.5–137.0 |
| Maintenance during ingest | 29.5–30.0 | 316.1–322.0 | 149.9–152.3 |

The loaded VictoriaMetrics process had variable idle RSS after restart, from
44.0 to 58.1 MiB in the controlled passes. Each timed stage has its own
baseline; a peak lower than a different stage's idle value is therefore not a
negative allocation. TinyStore's idle process is an in-process benchmark and
includes its harness. The servers' figures exclude their separate HTTP client.
Do not treat the row ratios as a direct library-versus-service efficiency claim.
Four of TinyStore's five eight-reader light runs peaked at 25.5–26.1 MiB; one
saved-revision run reached 31.9 MiB. The full range is retained in the table.

## Throughput and evidence of work

| Shape | TinyStore | VictoriaMetrics | Prometheus |
|---|---:|---:|---:|
| 1 light, queries/s | 10,576–11,205 | 2,972–2,986 | 1,983–2,073 |
| 8 light, queries/s | 8,979–9,493 | 6,438–6,535 | 5,336–5,526 |
| 1 native wide, queries/s | 29.4–29.6 | 7.7 | 71.3–80.3 |
| 2 native wide, queries/s | 42.8–44.1 | 13.7–13.8 | 207.7–212.4 |
| Ingest, samples/s | 267,225–275,145 | 2,403,861–2,617,658 | 2,126,551–2,217,906 |
| Maintenance during ingest, samples/s | 320,634–324,063 | 2,450,970–2,573,412 | 1,522,734–1,548,616 |

The throughput rows retain each API's own execution and acknowledgement
semantics. TinyStore ingest parses one JSONL series at a time in the same
process, ingests through the public API and calls `Maintain` after each series;
5,090,400 samples yielded 20,200 sealed blocks. The service ingest rows time
6,060 already-built remote-write requests of at most 1,000 samples, sent from
a separate client process; the timer excludes corpus parsing and protobuf
construction. Their server process RSS does not include the client.

The maintenance overlap uses a second, shared-clock chronological replay of
5,090,400 points in 5,292 requests so the server's time frontier advances
through seven hours. TinyStore runs `Maintain` concurrently with ingest;
20,190 blocks were sealed in each pass. Prometheus' logs show three `write block`
events and head GC during each ingest pass. VictoriaMetrics receives a forced
flush and merge after its inserted-row counter exceeds one million; trigger
logs recorded 2,474,400 and 2,800,600 rows respectively, and server logs
recorded merge completion in 17–18 ms. These maintenance mechanisms differ,
so the row proves overlap and measured RSS, not equivalent compaction work.

## Materializing a wide answer

The streaming read was also compared against each engine's materializing wide
path on the same selector and range. TinyStore `Read` peaked at 46.6–47.3 MiB
with one active query and 65.2–68.7 MiB with two. The `query_range` HTTP paths,
whose response bodies were consumed incrementally by the client but whose
server evaluation may materialize the matrix, peaked at 60.4–66.2 / 71.1–74.6
MiB for VictoriaMetrics and 202.6–203.6 / 284.1–299.9 MiB for Prometheus.
The native streamed protocols above answer a different server execution path;
Prometheus' large `query_range` versus streamed remote-read difference must not
be mislabelled as a property of HTTP response reading alone.

## Reproduce

Build `bench/perf` and its `remote` command in `golang:1.27`, store binaries on
one Linux volume and prepare the pinned TSBS JSONL there. Create TinyStore's
`read.db` with `TINYSTORE_CORPUS_DB=<volume>/read.db
TINYSTORE_JSONL=<corpus>/series.jsonl go test ./metrics -run
'^TestCorpusThroughPublicStore$' -v -count=1`. For each TinyStore read shape,
run a fresh process using:

```sh
<perf> -dir <volume> -stage tsbs_rss -shape idle -readers 8 -seconds 5
<perf> -dir <volume> -stage tsbs_rss -shape light -readers 1 -seconds 5
<perf> -dir <volume> -stage tsbs_rss -shape light -readers 8 -seconds 5
<perf> -dir <volume> -stage tsbs_rss -shape wide -readers 1 -seconds 5
<perf> -dir <volume> -stage tsbs_rss -shape wide -readers 2 -seconds 5
<perf> -dir <new-volume-directory> -stage tsbs_ingest_rss -corpus <corpus>/series.jsonl
<perf> -dir <another-new-directory> -stage tsbs_ingest_maint_rss -corpus <corpus>/series.jsonl
```

For VictoriaMetrics and Prometheus, start the pinned images with the settings
above, load the same corpus through `bench/perf/remote -stage write -batch 1000
-time-anchor-ms 1790119200000 -time-divisor 16`, then restart each server
before each idle/read shape. Run `remote -stage ram_query -shape light -workers
1|8 -seconds 5` for light reads, and `remote -stage native_stream -engine
vm|prom -workers 1|2 -seconds 5` for native wide reads. To sample process RSS
at 20 ms in a sidecar sharing its PID namespace:

```sh
docker run --rm --pid container:<server> alpine sh -c \
  'while :; do grep VmRSS /proc/1/status; sleep 0.02; done'
```

Stop the sampler after each timed stage and take the largest numeric `VmRSS`
value. For maintenance overlap, use fresh empty service files and
`remote -stage chronological_write -time-anchor-ms 1790124300000
-time-divisor 1`; run VM flush/merge when `vm_rows_inserted_total` passes one
million and inspect both servers' maintenance logs. Repeat the stages in
reverse order to expose warm-process drift. The exact commands in this report
refer to placeholders for prepared corpus and volume paths, never to a
machine-specific checkout.
