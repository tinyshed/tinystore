# Normalized records, context sharing and exact reconstruction — 2026-09-25

This round changes the research input from log lines to a common log/event
record. It compares complete row blocks, adaptive columns, reconstruction
rules with exceptions, and contexts shared across bounded microblocks.
`records/` itself is unchanged. [The design](../records.md) states what the
prototype does and what remains unbuilt.

Follow-up: [exact reconstruction and the ten-byte target](record-reconstruction-2026-09-25.md)
adds numerical/state/JSON candidates, a sixteen-microblock scope, a final
envelope, and a real structured-event control. The figures below remain the
earlier eight-microblock round, not the current best candidate.

## Environment and reproduction

- `spike/record_*_test.go` of commit `1bedb5d`, measured on the identical tree
  before it was committed on base `07341dec0b743c11b0557245aa345e3e309bd420`.
- Windows 11 Pro 10.0.26200, AMD Ryzen 7 7700, 8 cores / 16 logical processors.
  Go 1.27.1 windows/amd64, `modernc.org/sqlite` v1.59.0,
  `klauspost/compress` v1.19.0, zstd `SpeedDefault`, one encoder/decoder worker.
- Density only. Docker Desktop's Linux daemon was unavailable. These are not
  Linux throughput, query latency or peak-memory results.
- Eight synthetic fixtures, 10,000 records each, PCG(17,23), defined in
  `recordFixture`. Their generation is part of the checked-in harness.
- Ten Loghub 2k samples at `dd61d0952749ee7963bde24220d1be5ede023033`, checked
  against `bench/loghub-sha256.txt`. Corpus files are fetched and ignored.
- Every encoded block/segment is decoded and compared to the complete original
  normalized records in order. A fresh decoder is also tested in unit gates.
- SQLite uses a row per measured storage object and a `first_at` index.
  Each case is written in one transaction, then vacuumed; file size is
  `page_count * page_size`. `dbstat` reports bytes/pages by object. No WAL,
  mutable head, registry, search index or retention state is included.

```sh
TINYSTORE_SPIKE=1 go test ./spike -run '^TestAdaptiveRecordDensity$' -v -count=1
sh bench/run-record-events.sh <corpus>
go test ./spike -run '^TestRecord' -count=1
go test ./spike -run '^$' -fuzz '^FuzzRecordBlockDecode$' -fuzztime=20s -parallel=2
go test ./spike -run '^$' -fuzz '^FuzzRecordSegmentDecode$' -fuzztime=20s -parallel=2
task check
```

The measured Windows invocation, after downloading and checking the same
manifest, was:

```powershell
$env:TINYSTORE_SPIKE = '1'
$env:TINYSTORE_LOGHUB = '<corpus>'
go test ./spike -run '^TestAdaptiveRecordDensity$' -v -count=1
```

The shell runner above is supplied for reproduction; it was not itself run on
Linux in this round.

## Inputs and baselines

`frontend` chooses among 500 sessions, with stable per-session browser metadata
and a SHA256-derived 128-bit session identifier. Event coordinates, element,
URL and nanosecond jitter vary; timestamps can go backwards. `context_churn`
gives every event a new session. Neither fixture represents observed browser
traffic. `backend` has a fresh independent pseudorandom 128-bit trace ID per
record, random duration/status/user, and regular timestamps. `mixed` interleaves
browser, backend and unstructured multiline Java-like records.

`derived_fields` deliberately repeats one random user ID in an attribute,
route, URL and message; every 97th message is an exception. It demonstrates a
known dependency, not the prevalence of that dependency in real applications.
`regular_frontend` is a highly predictable toy with periodic coordinates,
regular time and sequential sessions. Its tiny result must not be presented as
a realistic frontend density. `shape_churn` introduces a unique key per event;
`noise` has 128 random bytes rendered as a hex body.

The small-block baseline is normalized binary rows under zstd, including
source context, names, optional fields and the integrity envelope. It is not
the production row table and not the earlier plain-line Loghub baseline.
Blocks stop at 1024 records, 256 KiB or the cell cap; all candidates use the
same boundaries. The block zstd window is 256 KiB.

Segments hold up to eight of those blocks. A second baseline concatenates
exactly the same normalized rows into one zstd frame with a 2 MiB window,
including a count and integrity envelope. Its decompressed bytes are checked
against the original row serialization. Giving this baseline the larger scope
separates structural savings from simply enlarging a compression block.

## Payload, bytes per record

Every column includes its own dictionaries, shape descriptions, references,
exceptions, frame headers and checksums. `columns` disables inter-column
prediction; `prediction` enables it. `shared segment` additionally lets the
encoder share contexts between microblocks.

| Fixture | Raw rows | Row zstd, block | Columns | Prediction | Row zstd, segment | Shared segment |
|---|---:|---:|---:|---:|---:|---:|
| frontend | 210.56 | 31.66 | 17.33 | 16.84 | 24.18 | 10.51 |
| backend | 189.51 | 34.54 | 20.47 | 20.46 | 34.17 | 20.42 |
| mixed | 150.42 | 28.11 | 17.92 | 17.76 | 23.96 | 14.94 |
| context churn | 210.55 | 42.83 | 24.85 | 24.83 | 42.88 | 24.83 |
| derived fields | 120.73 | 14.33 | 14.07 | 2.76 | 14.31 | 2.76 |
| shape churn | 43.95 | 3.27 | 3.27 | 3.27 | 3.23 | 3.28 |
| noise | 271.17 | 140.64 | 140.64 | 140.64 | 140.95 | 140.65 |
| regular frontend, toy | 210.61 | 17.28 | 0.69 | 0.69 | 16.88 | 0.55 |

The frontend gain from prediction alone is small: 17.33 to 16.84. Sharing the
context dictionary across blocks produces the larger second gain, to 10.51.
The dictionary's own column representation is included. Both frontend segments
choose sharing; both context-churn segments decline it.

Reconstruction is large only where the fixture contains a real dependency:
14.07 to 2.76 on derived fields, about a fivefold payload reduction. It stores
the independent random ID once and reconstructs three textual uses, including
the exception stream. It does not recover arbitrary missing information.

Backend has 16 bytes of independent trace information per event. Its 20.42-byte
result leaves limited room for another multiplicative improvement on this
particular generator. No ID is shortened or dropped.

Independent blocks never exceed their complete small-block row baseline.
Segments compare two layouts that retain microblocks; they do not choose the
one-frame larger baseline. Consequently shape churn loses against that larger
frame. Even declined sharing adds a small segment wrapper. There is no claim
that every input beats every zstd block size.

## Complete SQLite files, bytes per record

Segment rows use fewer time-index entries than independent block rows. Comparing
the two segment columns keeps that physical granularity equal. Page packing is
part of the measured result, not an inferred payload-to-file conversion.

| Fixture | Row zstd, block | Adaptive block | Row zstd, segment | Shared segment |
|---|---:|---:|---:|---:|
| frontend | 33.59 | 18.43 | 25.80 | 11.47 |
| backend | 37.27 | 22.12 | 35.23 | 21.30 |
| mixed | 29.90 | 19.66 | 25.40 | 16.38 |
| context churn | 45.06 | 26.21 | 44.24 | 26.21 |
| derived fields | 17.20 | 5.32 | 15.16 | 3.69 |
| shape churn | 5.32 | 5.32 | 4.10 | 4.92 |
| noise | 142.13 | 142.13 | 142.54 | 142.13 |
| regular frontend, toy | 19.25 | 2.05 | 18.43 | 1.64 |

Representative `dbstat` division, raw bytes. Each entry is allocated / payload /
unused; b-tree headers and cell pointers account for the remainder.

| Fixture and layout | blocks | blocks_at | sqlite_schema |
|---|---:|---:|---:|
| frontend, row segment | 249856 / 241884 / 7685 | 4096 / 23 / 4059 | 4096 / 244 / 3735 |
| frontend, shared segment | 106496 / 105176 / 1192 | 4096 / 23 / 4059 | 4096 / 244 / 3735 |
| backend, row segment | 344064 / 341747 / 1957 | 4096 / 23 / 4059 | 4096 / 244 / 3735 |
| backend, shared segment | 204800 / 204213 / 363 | 4096 / 23 / 4059 | 4096 / 244 / 3735 |
| derived fields, row segment | 143360 / 143163 / 33 | 4096 / 9 / 4073 | 4096 / 244 / 3735 |
| derived fields, shared segment | 28672 / 27668 / 953 | 4096 / 9 / 4073 | 4096 / 244 / 3735 |

The harness prints this breakdown for every file. It also accounts for every
adaptive-block byte as envelope, compressed directory, or column data. For
frontend these are 0.014 / 0.194 / 16.633 bytes per record; their sum is the
16.84 block result before segment sharing.

## Plain-text control

Each pinned Loghub line is an opaque body with its dataset as stream and its
ordinal as a synthetic event timestamp. The ordinal is not a parsed original
timestamp. No template mining or full-file training is performed. All ten
datasets contain 2000 records, so their tiny file sizes are dominated by page
rounding; payload is the useful control here.

| Dataset | Row zstd, block | Adaptive block | Row zstd, segment | Adaptive segment |
|---|---:|---:|---:|---:|
| Android | 16.35 | 13.16 | 14.88 | 13.17 |
| Apache | 9.31 | 5.59 | 9.22 | 5.60 |
| BGL | 35.87 | 32.29 | 35.84 | 32.30 |
| HDFS | 32.61 | 28.32 | 32.73 | 28.33 |
| Hadoop | 12.54 | 9.71 | 12.00 | 9.71 |
| Linux | 10.52 | 7.99 | 10.45 | 8.00 |
| OpenSSH | 12.14 | 8.99 | 12.43 | 9.00 |
| Spark | 11.21 | 7.72 | 10.98 | 7.73 |
| Thunderbird | 19.59 | 15.87 | 19.67 | 15.88 |
| Zookeeper | 16.53 | 13.45 | 16.11 | 13.46 |

These figures are not a win over the earlier typed Loghub codec: that baseline
stored different data. They show that the common event model can carry legacy
text without requiring a text-specific compressor or giving up its exact body.

## Gates and remaining work

`task check` passed on Windows, including the module checks, formatting, lint,
all tests, vulnerability scan and the cgo-free Linux/amd64 size build. The size
probe was built for Linux, not executed as a Linux runtime test. Focused record
tests cover all four layouts, scalar extremes, null/absence, multiline and
binary bodies, late/duplicate timestamps, ownership, limits, reconstruction
exceptions, corrupt checksums, malformed dictionaries and expansion bounds.
Decoder fuzz targets also repair the outer checksum before decoding mutations,
so their job is not limited to rejecting checksum mismatches.
The final 20-second runs completed 588,641 block and 531,302 segment executions
with two workers, without a failure. Segment seeds include both independent
blocks and an explicit shared dictionary.

Still unmeasured: a real structured-event corpus, sustained encoder CPU,
allocations and peak RSS, selective decode and filter latency, ingestion/WAL
cost, partial retention, context ownership after deletion, durable head and
publication recovery. The source adapter is a prototype JSON envelope and a
native test type, not an HTTP endpoint or a released protocol. Nested values
are preserved as JSON fragments rather than recursively columnarized.

The larger segment is a bounded dictionary scope, not a free improvement: it
buffers more input and this decoder validates/decodes the whole segment.
Selective integrity and independently reclaimable microblocks are acceptance
gates before engine integration.
