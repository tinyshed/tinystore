# Sealing, the head, late records, id blooms and memory — 2026-09-25

The last questions the research owed the engine before it is built: when a
head seals a segment, what the head itself costs, what late records do to
reads, how a lookup by an id-like attribute prunes, and how much memory one
segment needs. Measured on the production container logs of
[the previous round](record-docker-logs-2026-09-25.md) where a real arrival
pattern matters, and on the synthetic frontend fixture where a controlled
lateness does.

## Environment and reproduction

Commit `fed6ea9`; Windows 11 Pro 10.0.26200, AMD Ryzen 7 7700, Go 1.27.1
windows/amd64, `modernc.org/sqlite` v1.59.0, `klauspost/compress` v1.19.0,
1 KiB pages. The container corpus is the private snapshot
`6bf41f814539fdced24d5403e3adc27939446028717d006e63946601695d1ea0`.

```sh
TINYSTORE_SPIKE=1 TINYSTORE_RECORD_DOCKER=<corpus> \
  go test ./spike -run '^TestRecordV2SealPolicy$|^TestRecordV2HeadFlushes$|^TestRecordV2DockerLogs$' -v -count=1 -timeout 60m
TINYSTORE_SPIKE=1 go test ./spike -run '^TestRecordV2LateRecords$|^TestRecordV2SegmentMemory$' -v -count=1
```

## A quiet stream must wait an hour before it seals

Most of the 65 containers write well under one line a second, so a segment of
16,384 records would take days to fill. A head that also seals when its oldest
record reaches an age cuts every stream's arrivals like this:

| Seal when full or older than | Segments | Median records | Payload B/record | File B/record |
|---|---:|---:|---:|---:|
| 1 minute | 120,112 | 6 | 47.559 | 66.234 |
| 10 minutes | 21,251 | 24 | 24.032 | 29.275 |
| 1 hour | 4,781 | 36 | 20.394 | 21.962 |
| 6 hours | 1,072 | 144 | 20.107 | 20.888 |
| 24 hours | 378 | 402 | 19.639 | 20.245 |
| only when full | 152 | 10,217 | 19.530 | 20.086 |

Sealing after an hour costs 9 % of the file, after a day 0.8 %. Small segments
pay their own names, shapes, dictionaries and column headers; a merge of a
stream's small sealed segments can later recover most of the difference.

## The head is rows, not segments

Written once a second per stream, as the current handler flushes, the head
receives 632,070 flushes of 2.1 records on average:

| One flush as | B/record |
|---|---:|
| plain rows | 249.6 |
| rows under zstd | 129.5 |
| a v2 segment of its own | 152.4 |

A v2 segment does not pay for itself at two records. The head keeps a row per
flush under zstd and holds at most an hour or a segment's worth of a stream:
for this fleet about four thousand records, half a megabyte. Every record is
written twice, once to the head and once sealed.

## Late records need a head of their own

One frontend record in a hundred moved up to ten minutes into the past, as a
client that was offline delivers it, one million records, 100 reads of one
second each:

| Records | Blocks | Widest block | Blocks read per second | Rows per read | Payload B/record |
|---|---:|---:|---:|---:|---:|
| on time | 977 | 1.8 s | 1.92 | 810 | 7.6887 |
| 1 % up to 10 minutes late | 977 | 10 min | 26.08 | 808 | 7.7596 |
| the same, late ones in their own head | 986 | 6.1 min | 5.12 | 808 | 7.7363 |

Sorted by event time, a segment's late records land in its first block and
stretch it over ten minutes: every short read in that window reads the first
block of thirty segments. Moving what a batch holds from more than a minute
before its median into a late head per stream, sealed the same way, cuts the
reads fivefold and stores slightly less. The late blocks still span minutes;
merging them into the segments of their time would take the rest.

## Id-like attributes get a bloom per block

An attribute column gets a bloom filter per block, 10 bits a distinct value,
when it holds JSON strings of at most 64 bytes of which nine in ten are
distinct: request ids, hashes, uuids. Distinct numbers are times and measures
a range asks for, and get none. Filters live in
`block_filters (key, block, bloom)`, keyed for lookup by key.

| On the container logs | Blocks read | Bytes | Time |
|---|---:|---:|---:|
| one `requestId`, no filter | 160 | 3,725,982 | 43 ms |
| one `requestId`, with filters | 11 | 1,232,981 | 21 ms |

The filters cost the services writing a request id per line 1.24 bytes per
record; the whole corpus goes from 19.9545 to 20.0860 bytes per record in the
file, and the synthetic fixtures do not change. Most of the remaining bytes are
segment rows read to learn whether a segment has the key at all; an index of
keys per segment removes them.

## One segment's memory

Cumulative allocation for a full segment, the bound a reservation has to cover:

| Segment | Records | Input | Encode | Decode |
|---|---:|---:|---:|---:|
| frontend | 16,384 | 3.9 MiB | 12.1 MiB | 16.0 MiB |
| backend | 16,384 | 3.5 MiB | 17.0 MiB | 20.6 MiB |
| random hex bodies | 14,266 | 4.0 MiB | 11.6 MiB | 15.3 MiB |

Encoding allocates three to five times a segment's input and decoding four to
six; at the 4 MiB input limit a reservation of 24 MiB per segment covers both.
This is allocation, an upper bound on live heap, not a measured peak RSS.

## Throughput on Linux

The earlier figures were measured on Windows. The same benchmarks in the
`golang:1.27` container (go1.27.1 linux/amd64, Docker Desktop on the same
machine, module cache mounted read-only, `GOPROXY=off`), one CPU:

```sh
docker run --rm -v <repo>:/src -v <module cache>:/go/pkg/mod -w /src -e GOTOOLCHAIN=local \
  -e GOFLAGS=-mod=readonly -e GOPROXY=off -e GOWORK=off golang:1.27 go test ./spike -run '^$' \
  -bench '^BenchmarkRecordV2Throughput$|^BenchmarkRecordThroughput$/adaptive' -benchtime=20x -benchmem -cpu=1 -count=1
```

| Path | Records/s | Allocated per record |
|---|---:|---:|
| previous `final` encode, 10,000 records | 950 | 848 KB |
| previous decode | 412,294 | 3.4 KB |
| v2 frontend encode, 16,384 records | 404,961 | 758 B |
| v2 frontend decode | 1,512,787 | 1.0 KB |
| v2 backend encode | 664,698 | 998 B |
| v2 backend decode | 1,349,746 | 1.3 KB |

## What this settles, and what it does not

- Head: a row per stream and flush, under zstd. Seal: 16,384 records, 4 MiB,
  or an hour of age; a late head per stream for what arrives more than a
  minute behind its batch.
- Layout: `segments`, `blocks` with a covering time index, `block_traces` and
  `block_filters`; 1 KiB pages; a reservation of 24 MiB per segment in flight.
- Not measured: a merge of small sealed segments, an index of keys per
  segment, a bound on the time-index scan with late blocks present, recovery of
  a head after a crash, concurrent readers, and density or queries on Linux.
