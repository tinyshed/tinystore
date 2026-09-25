# The records engine on the research corpora — 2026-09-25

The records engine is built to [docs/records.md](../records.md), and this
round measures it with the fixtures and the corpus the research used, against
the numbers [the hand-off](records-research-2026-09-25.md) said it must hold.
Every figure comes from the engine itself: records appended through
`records.Append`, sealed by `Maintain`, read by `Read`, the file divided by
`dbstat`. The runner is `bench/records`.

Follow-up: [what a late record is behind](records-late-reference-2026-09-25.md)
measures a late record against its stream's newest record instead of its
batch's median: one-second reads with late records fetch 4.63 blocks in the
research's batches and 5.24 in flushes, and a record appended alone is late
again. [The rice parameter round](records-rice-2026-09-26.md) chooses each rice
column's parameter among all 64 and takes the production corpus from 20.8984
to 20.7738 bytes a record in full segments, and from 22.9463 to 22.8333 sealed
hourly; the frontend fixture does not move.

## Environment and reproduction

- The engine at `49dc7b1` and its runner at `b646acd`, branch
  `records-engine` from `5294e36`.
- AMD Ryzen 7 7700, 8 cores / 16 logical processors, Windows 11 Pro
  10.0.26200, Docker Desktop 29.6.2; every figure below ran in the
  `golang:1.27` container (go1.27.1 linux/amd64) with the module cache mounted
  read-only and the store on a tmpfs. `modernc.org/sqlite` v1.59.0,
  `klauspost/compress` v1.19.0, 1 KiB pages.
- Fixtures: `frontendRecords` of `bench/records`, the research's
  `recordFixture("frontend")` with its seeds, one million records; the late
  variant moves one record in a hundred up to ten minutes into the past with
  the research's seeds.
- Corpus: the private production snapshot
  `6bf41f814539fdced24d5403e3adc27939446028717d006e63946601695d1ea0`, 65
  containers on two hosts, read as the research read it: docker's receive
  time, the container as the stream, docker's stream as the name, a JSON line
  that rebuilds byte for byte as attributes and any other line as its body.
  1,323,561 records, 2 entries skipped for NUL padding. Aggregates only.

```sh
docker run --rm -v <repo>:/src -v <module cache>:/go/pkg/mod:ro -w /src/bench/records \
  -e GOWORK=off -e GOTOOLCHAIN=local -e GOFLAGS=-mod=readonly -e GOPROXY=off \
  --tmpfs /data:rw,size=6g golang:1.27 sh -c 'go build -o /tmp/records . &&
  /tmp/records -stage density -dir /data/a &&
  /tmp/records -stage late -batch 16384 -dir /data/b &&
  /tmp/records -stage late -batch 1024 -dir /data/c &&
  /tmp/records -stage docker -corpus <corpus> -dir /data/d &&
  for age in 1h 6h 24h; do /tmp/records -stage replay -seal-age $age -corpus <corpus> -dir /data/r$age; done'
docker run --rm -v <repo>:/src -v <module cache>:/go/pkg/mod:ro -w /src \
  -e GOWORK=off -e GOTOOLCHAIN=local -e GOFLAGS=-mod=readonly -e GOPROXY=off golang:1.27 \
  go test ./records -run '^$' -bench '^BenchmarkSegment$' -benchtime=20x -benchmem -cpu=1 -count=6
```

`density` and `late` append the fixture in arrival order, `-batch` records a
call, maintain whenever a segment's worth has arrived, then move the clock two
hours to seal what waits. `docker` appends each container's records a minute's
worth at a time, so that a head seals only when full, as the research cut its
segments. `replay` runs the whole corpus on its own clock: each minute's
records appended together, `Maintain` every minute, so a head seals when full
or `-seal-age` old. Each stage vacuums the closed file before dividing it.

## Against the numbers the engine must hold

| | Must hold | Prototype | Engine | |
|---|---:|---:|---:|---|
| one million frontend records, B/record in the file | 7.9 | 7.8531 | 7.8899 | holds |
| the production corpus, B/record in the file | 20.1 | 20.0860 | 20.8984 | does not: the text sample is left out |
| encoding, frontend records/s on one CPU | 400,000 | 404,961 | 567,256 to 707,513 | holds |
| decoding, frontend records/s on one CPU | 1,300,000 | 1,512,787 | 1,291,089 to 1,438,152, median 1,384,297 | holds at the median; one run of six below |
| allocated per encoded record | 1 KB | 758 B | 663 B | holds |
| blocks a one-second read of a million records fetches | 2 | 1.92 | 1.92 | holds |
| blocks one request id fetches in the production corpus | 11 | 11 | 11 | holds |
| blocks a one-second read fetches with 1 % late | 5.2 | 5.12 | 5.12 at the research's batches, 5.26 in flushes | holds as measured there |

## Where the bytes are

`dbstat`, bytes a record, pages of each b-tree:

| | Frontend | Production, full segments | Production, hourly |
|---|---:|---:|---:|
| blocks | 7.2540 | 20.6833 | 21.9428 |
| segments | 0.5693 | 0.0139 | 0.3559 |
| block_filters | — | 0.1315 | 0.2886 |
| blocks_by_time | 0.0369 | 0.0472 | 0.1470 |
| segment_keys | 0.0133 | 0.0054 | 0.1393 |
| segments_by_end | 0.0010 | 0.0031 | 0.0588 |
| the rest: schema, streams, empty head tables, a page each | 0.0154 | 0.0139 | 0.0139 |
| the file | 7.8899 | 20.8984 | 22.9463 |

On the frontend fixture the blocks and segment rows take 7.8233 bytes, the
prototype's encoding. The rest is what the engine keeps beside them: a time
index covering seven columns where the prototype's covered three, so that a
query chooses its blocks without reading a row, the keys each segment holds,
and tables a record in flight passes through.

## The text sample is worth 0.80 bytes a record

The engine writes JSON lines to the byte the prototype did: the containers
whose lines are mostly JSON cost 21.8602 bytes a record in payload, the
prototype's 21.53 and 42.00 weighted by their records. The text containers
cost 20.1584 against 19.28. Their blocks grew by 1.57 bytes a record and their
segment rows shrank by 0.79, the 64 KiB sample each text-heavy segment kept;
the corpus pays 0.80 bytes a record more, 20.8984 against 20.0860. The
docker-logs round counted the sample's gain at 0.37 bytes a text record from a
comparison of body columns alone; the whole file shows more than twice that.
The engine left the sample out for the decoder state a segment would need, as
the hand-off decided, so the corpus's figure is not held.

## Late records and the size of a batch

A record more than a minute behind the median of its batch goes to the late
head. With the fixture appended 16,384 records a call, as the research cut its
batches, one-second reads fetch 5.12 blocks; appended 1,024 a call, as the
handler flushes, 5.26. Both files have 986 blocks and a widest block of 6
minutes 6 seconds, and the same bytes. The median of a short batch sits closer
to its records, and separates the late ones slightly worse.

## Sealing on the corpus's own clock

The same records, a head sealed when full or `SealAge` old:

| Sealed when full or | Segments | Records a segment: quartiles | In segments under 100 | B/record in the file |
|---|---:|---:|---:|---:|
| an hour old | 4,620 | 18 / 38 / 363 | 5.1 % | 22.9463 |
| six hours old | 1,032 | 33 / 146 / 2,097 | 1.0 % | 21.3309 |
| a day old | 365 | 80 / 394 / 8,373 | 0.3 % | 20.9951 |
| full only | 153 | 193 / 9,610 / 16,375 | 0.1 % | 20.8984 |

At an hour the research cut 4,781 segments of a median of 36 records and
paid 21.962 bytes a record with the sample. The sample accounts for 0.8 of the
difference; the rest is what a segment pays whatever its size: its keys cost
0.1393 bytes a record and its time index 0.1470, where full segments pay
0.0054 and 0.0472. Waiting six hours takes back four fifths of what an hour
costs and a day nearly all of it, at the price of records waiting longer in
the head, where [the sealing round](record-sealing-2026-09-25.md) measured a
record at about six times its sealed bytes, and reaching `Follow` later. Merging a stream's small sealed segments would keep the hour
and the bytes, if a follower's cursor can survive the merge.

## Queries on the production corpus

Each result was compared with a count over the corpus. Single runs, warm cache.

| Query | Rows | Blocks | Bytes | Time | Research |
|---|---:|---:|---:|---:|---|
| the busiest minute, every container | 4,770 | 36 | 460,867 | 6.2 ms | 35 blocks, 502,750 bytes, 9.4 ms |
| one `requestId`, all time | 1 | 11 | 42,483 | 4.8 ms | 11 blocks, 1,232,981 bytes, 21 ms |
| `level = 50` inside JSON, all time | 31 | 186 | 2,874,452 | 14.4 ms | 186 blocks, 3,906,056 bytes, 35 ms |

The request id fetches a thirtieth of the research's bytes: the research read
every segment row to learn whether a segment had the key, and the engine asks
its segment keys instead.

## Speed

`BenchmarkSegment`, one CPU, six runs of twenty segments of 16,384 records:

| | Records/s | Allocated per record |
|---|---:|---:|
| frontend encode | 567,256 to 707,513 | 663 B |
| frontend decode | 1,291,089 to 1,438,152 | 1.5 KB |
| backend encode | 763,404 to 800,963 | 1.0 KB |
| backend decode | 1,357,239 to 1,508,246 | 1.5 KB |

A first version decoded at 1.13 to 1.30 million records a second: every value
was a string of its own, 105,000 allocations a segment. A column's values now
share one string and a block's lists share chunks, 4,335 allocations a segment;
a page copies the records it keeps, so it does not hold the columns they came
from.

Appending and sealing the million frontend records, the SQLite writes
included, took 3.6 seconds, 280,000 records a second.

## What this settles, and what it leaves

- Where the engine takes the prototype's encoding it reproduces its figures:
  the frontend file, the JSON services, the one-second reads, the request id
  and the late records at the research's batching.
- The production corpus's 20.1 is not held without the per-segment text
  sample; bringing it back, or reaching the same bytes with text templates, is
  a decision rather than a measurement.
- A late record's reference could be the head's newest time rather than its
  batch's median.
- Tested, not measured: that a reader finds each record once while appends
  and sealing run beside it is a test, under the race detector on Linux; what
  reads cost under that load, and the process's memory and write-ahead log
  while it lasts, are not measured. Nor is any figure on Linux outside a
  container or on macOS.
