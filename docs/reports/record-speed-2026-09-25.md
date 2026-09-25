# Normalized record codec: quick throughput probe — 2026-09-25

The density candidate is expensive to encode in its current exhaustive research
implementation: about 965 records/s on one Go CPU. Decoding reconstructs about
368,000 records/s. This is an in-memory codec measurement, not SQLite ingest
throughput, durable flush speed, a sustained service rate or a latency percentile.

Follow-up: [the v2 round](record-v2-2026-09-25.md) profiles this encoder and
replaces it with one that chooses representations by computed size.

## Environment and method

Spike sources of commit `1bedb5d`, measured on the identical tree before it was
committed on base `07341dec0b743c11b0557245aa345e3e309bd420`. Windows 11 Pro,
AMD Ryzen 7 7700, Go 1.27.1 windows/amd64, `klauspost/compress` v1.19.0.
`-cpu=1` sets GOMAXPROCS to one; zstd encoder/decoder concurrency is also one.
Each timed operation processes the entire unchanged 10,000-event frontend
fixture from the [density acceptance test](record-reconstruction-2026-09-25.md):
2,105,558 normalized input bytes, with its recorded SHA-256 unchanged.

The adaptive encoder uses all candidate families, sixteen-microblock scope and
the final strong envelope. The two references serialize the same logical records
and use zstd `SpeedDefault` or `SpeedBestCompression` with a 4 MiB window.
All decoders reconstruct record objects and validate fields; checksums and
serialization work are included. Exact comparison against the original records
happens outside the timed region. Dataset creation, codec initialization,
preparing the decode input, and SQLite I/O are excluded. Codec instances are
reused. This is not a byte-only zstd decompression benchmark.

One short run, three timed operations per sub-benchmark; allocation counts come
from Go's benchmark counters. The process took 53.7 seconds including untimed
preparation and benchmark calibration. Do not interpret these three repetitions
as a stable p99 or a reliable ranking between the two quick baseline decodes.

```sh
go test ./spike -run '^$' -bench '^BenchmarkRecordThroughput$' \
  -benchtime=3x -benchmem -cpu=1 -count=1
```

## Result

MB/s uses uncompressed normalized input bytes. Allocated MB is cumulative
allocation per complete 10,000-record operation, not live heap or peak RSS.

| Path | Seconds / 10,000 records | Records/s | Input MB/s | Allocated MB/op | Allocations/op |
|---|---:|---:|---:|---:|---:|
| adaptive encode | 10.358742 | 965 | 0.20 | 8482.56 | 58,855,355 |
| adaptive decode | 0.027159 | 368,202 | 77.53 | 34.08 | 327,943 |
| zstd default encode | 0.011688 | 855,554 | 180.14 | 13.52 | 100,846 |
| zstd default decode | 0.015022 | 665,677 | 140.16 | 19.19 | 321,826 |
| zstd best encode | 0.034014 | 293,994 | 61.90 | 13.50 | 100,845 |
| zstd best decode | 0.011397 | 877,439 | 184.75 | 19.19 | 321,823 |

The current adaptive encoder is about 304 times slower than the strongest
row-zstd reference in this probe. Its roughly 828 KiB of cumulative allocation
per record is also substantial. A bounded live working set does not imply cheap
allocation or GC. The 9.8304 B/record density result remains valid, but does not
establish a useful hot-path encoder.

The source visibly retries layouts, context representations, column codecs,
numeric predictors and segment organization. A profile is still needed to
attribute the measured time and allocation to individual stages. No encoder
optimization was mixed into this timing round. Likely next experiments are
bounded candidate screening, parsing a column once, reuse of selected modes,
and separating occasional model selection from routine encoding; no speedup
from those is claimed here.

The benchmark and existing code compiled and passed exact reconstruction
checks. `golangci-lint run ./spike` passed. The full project suite was not rerun
for this benchmark-only addition; it passed in the preceding density round.
