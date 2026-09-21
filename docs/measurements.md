# The measurements

Every number this design rests on, with what produced it. A figure without its
environment is an anecdote, so each section says what the fixture was, how many
samples it held and what was excluded from the file it measured.

Unless a section says otherwise: `linux/amd64`, Go 1.27, `modernc.org/sqlite`
v1.59, `klauspost/compress` v1.19, on a Ryzen 7 7700. Every SQLite file was
measured after `pragma wal_checkpoint(truncate)`, or the number would include
whatever the write-ahead log happened to be holding. Heavy measurements run in
a linux container, or they cannot be compared with the rest:

```sh
docker run --rm -v <repo>:/src -w /src -e TINYSTORE_SPIKE=1 golang:1.27   go test ./spike -run <name> -v -count=1
```

The codec's own gates need no container and no flag:

```sh
go test ./codec
go test ./codec -run '^$' -fuzz FuzzDecode -fuzztime=20s -parallel=2
```

## The adaptive codec against the one it replaced

Docker `golang:1.27`, linux/amd64, modernc SQLite 1.59.0, zstd 1.19.0. The codec
fixture uses 64 seeded blocks per workload and compares identical inputs with
the original varint-time/raw-float/zstd encoder. Times are averages of 200 calls,
not concurrent throughput. Payload figures include the new envelope and checksum.

| 240-sample workload          | old payload B/sample | adaptive | encode / decode |
|------------------------------|----------------------|----------|-----------------|
| constant                     | 0.171                | 0.142    | 3.81 / 1.22 us  |
| integer walk                 | 1.266                | 0.671    | 23.96 / 1.66 us |
| integer counter              | 1.424                | 0.808    | 22.18 / 1.93 us |
| temperature, tenths          | 1.719                | 1.792    | 25.67 / 5.38 us |
| sine                         | 8.117                | 7.946    | 15.26 / 8.47 us |
| noisy float walk             | 7.415                | 6.644    | 18.01 / 7.11 us |
| random IEEE-754 bits         | 8.142                | 8.108    | 15.53 / 1.71 us |
| integer walk, jittered times | 3.538                | 2.590    | 23.11 / 2.47 us |

At 8/16 samples, integer payloads changed from 7.342/4.793 to 5.230/2.625
bytes per sample. Temperature changed from 9.125/6.312 to 10.625/7.085. That
regression was real, and it was the hole scaled decimals were written to close;
the section below is the same fixture after they existed. Chimp and lossy
transforms are still not implemented.

The SQLite comparison writes 10 000 blocks of 240 samples per case, with foreign
keys and expiry/reference indexes enabled. Main-file sizes are measured after
checkpoint and exclude registry, head and WAL. The shared-key candidate changes
the clustered block table to a rowid table with a unique `(series_id,start_ts)`
index, and gives the payload the same integer PK with `ON DELETE CASCADE`:

| workload        | old codec, existing schema | adaptive, existing schema | adaptive, shared key |
|-----------------|----------------------------|---------------------------|----------------------|
| integer walk    | 1.809 B/sample             | 1.152                     | 1.101                |
| integer counter | 1.966                      | 1.263                     | 1.212                |
| noisy float     | 9.124                      | 9.124                     | 9.062                |

The noisy payload shrank, but both sizes still occupy the same number of SQLite
pages in this fixture. Summary scans of 10 000 blocks averaged 0.761/0.776 ms
for integer existing/shared-key tables, 0.768/0.781 ms for counters and
0.897/0.904 ms for noisy floats. Cascade cleanup was checked. Shared keys are a
promising schema candidate; these are not production migrations or range-query
benchmarks, and changing clustering still needs representative range reads.

## Decimals as the integers they were written as

Sizes are platform-independent and were measured on Windows, on the same
fixtures as the section above; the times were measured there too and are
comparable only with each other, not with the docker numbers.

| 240-sample workload | before the scaled path | after | what it chose |
|---------------------|------------------------|-------|---------------|
| temperature, tenths | 1.792 B/sample         | 0.379 | scaled        |
| everything else     | unchanged              |       | unchanged     |

Five decimal shapes, 240 samples each, all of which the encoder chose the
scaled representation for and every one of which round-trips bit for bit:

| written as             | payload        |
|------------------------|----------------|
| tenths of a degree     | 0.379 B/sample |
| hundredths, walking    | 0.258          |
| money, two places      | 0.287          |
| a percentage in tenths | 0.229          |
| thousandths, walking   | 0.275          |

What it costs to look for a scale, measured against the same code without it,
300 encodes of a 240-sample block on a Ryzen 7 7700:

| workload | encode before | after   |
|----------|---------------|---------|
| integer  | 23.6 us       | 25.0 us |
| float    | 18.0 us       | 18.9 us |

Decoding is unchanged: a scaled sample costs one division more than an integer
one. The product is rounded rather than truncated, and that is not a detail:
573 of the 9999 two-place decimals multiply to just under the integer they were
written as, so truncating would refuse a 240-sample block of them with
probability about one minus seven in ten million. The first version of this had no ceiling on the scale, and a noisy float
walk — where every value happens to have a thirteen-digit decimal form that
divides back exactly — took **95.9 us** to encode a block it then discarded for
being too large. Capping the scale at nine places returned that to 15.3 us and
changed no payload size anywhere, which is the measurement behind the ceiling
rather than a taste for round numbers.

In the file, with the schema unchanged and both maintenance indexes still in
place, `TestWhatASummaryCostsWhenItIsNotWholeNumbers` on 10 000 blocks:

| workload            | before         | after |
|---------------------|----------------|-------|
| temperature, tenths | 2.342 B/sample | 0.865 |

Dropping the foreign-key index, which is the removal that owes nothing, puts
that at 0.783; dropping the expiry index as well would put it at 0.682, with
the debt described in [research.md](research.md).

## Immutable microchunks against a row head

Small historical blocks and microchunks have different overhead. The new
experiment stores a microchunk inline in one `WITHOUT ROWID` row with
`series_id,start_ts,end_ts,count,body`, without full summaries or a payload table.
Both use the implemented codec. Only strictly safe samples are packed, and
`sealed_before` advances with the move from head to chunk in one transaction.

The experiment runs 10 000 synchronised daily series for 90 days with 30-day
retention and the same integer values as the head baseline. It uses 500-series
transactions, one per-series GC timestamp covering both head rows and chunks,
and validates exact retained samples from a snapshot throughout the run.
Encoding is inside the writer in this serial prototype; optimistic off-writer
compaction and concurrent late arrival are not established by this experiment.

| representation     | physical peak | projection to 1M + old registry | WAL bytes/input sample | warm read p50/p99 |
|--------------------|---------------|---------------------------------|------------------------|-------------------|
| row head           | 6.375 MiB     | 771.8 MiB                       | 641.4                  | 24.9 / 87.7 us    |
| immutable chunk=8  | 4.859 MiB     | 620.2 MiB                       | 268.5                  | 21.0 / 80.7 us    |
| immutable chunk=16 | 5.027 MiB     | 637.0 MiB                       | 297.9                  | 27.0 / 113.0 us   |

The projection adds the earlier 134.3 MiB registry; that registry was not rebuilt
here. WAL autocheckpoint is disabled for accounting, each simulated day starts
with a truncated WAL, and the file length before the next checkpoint is summed.
These are serialized WAL bytes under that batching policy, not SSD write bytes
or a proposed production checkpoint schedule. Reads are 100 warm single-series
queries at the end; they do not establish concurrent query p99. The run takes
8.4/11.0/9.6 seconds for head/chunk8/chunk16, respectively.

Chunk8 saves about 20% total projected disk and 58% of WAL bytes on this fixture.
That is useful but below the research's hoped-for 35% total disk reduction; it
does not justify making a third storage level mandatory yet. It remains a spike
until sparse-to-dense transitions, idle-series expiry, concurrent late writes
and mixed values prove the complexity worthwhile. Mutable BLOBs are unmeasured.

## Where the value stream still has meat

`TestWhereTheValueStreamStillHasMeat`, 64 blocks of 240 samples per workload,
the same fixtures as everywhere else, measured on Windows. Every column is bits
per delta over the block's ZigZag deltas, and every compressed column includes
whatever table or frame that coder needs, one per block — because one per block
is what the codec writes.

| workload            | simple8b | fixed width | zstd | huff0 | order-0 entropy | distinct symbols |
|---------------------|----------|-------------|------|-------|-----------------|------------------|
| integer walk        | 4.26     | 4.00        | 8.44 | 3.36  | 3.17            | 9                |
| integer counter     | 5.36     | 5.00        | 8.44 | 3.77  | 3.46            | 11               |
| temperature, tenths | 1.87     | 2.00        | 2.74 | 1.71  | 1.56            | 3                |
| integers, jittered  | 4.24     | 4.00        | 8.44 | 3.36  | 3.17            | 9                |

Four things.

**Simple8b pays for the widest value in each word**, so a fixed width over the
whole block is 0.26 to 0.36 bits a delta cheaper on three of the four. On
temperature it is the other way round, because long runs of narrow values let
the selector take a more capacious word. Neither wins everywhere, which is an
argument for both being candidates rather than for replacing one with the other.

**zstd refuses to entropy-code a block this small.** Fed one byte per delta it
returns 8.44 bits a delta — the 239 bytes stored raw plus a 13-byte frame. So
"let the general compressor do the entropy coding" does not work at 240 samples,
which is worth knowing before anyone proposes it again.

**`huff0` lands within 6 to 9 percent of the order-0 entropy, table included**,
and it is not a new dependency: it is a package of `klauspost/compress`, which
is already in the module for zstd. On a counter that is 5.36 bits a delta down
to 3.77, which is 0.199 bytes a sample off the payload.

**The bound itself is the news.** The gap between what we write and the order-0
entropy of the delta alphabet is 20 to 35 percent, on alphabets of three to
eleven symbols. That is larger than anything the XOR-family candidates are
expected to find on the same data.

Two corrections to this measurement, both caught by rerunning it rather than by
reading it. The first version compared the width of each delta on its own,
which is not what Simple8b charges; the second compressed all 64 blocks as one
stream, so zstd found repetition between blocks that a per-block encoder cannot
have, and reported a triumphant 0.05 bits a delta for temperature.

## Where a dense block's bytes actually are

`TestWhereADenseBlocksBytesAre`, 10 000 blocks of 240 whole-number samples,
1 000 series, written in one transaction and measured after a checkpoint. The
breakdown is `dbstat`, which reports each b-tree's own pages, so it is a
division of the file rather than a difference between runs. Sizes do not depend
on the platform, so this one was run on Windows; the timings are warm local
runs of `select sum(count), sum(sum) from blocks`, twenty of them, and are
comparable only with each other.

| layout                                         | file           | summary scan |
|------------------------------------------------|----------------|--------------|
| as measured                                    | 1.147 B/sample | 0.73 ms      |
| summary columns declared `integer`             | 1.147          | 0.73 ms      |
| no summary columns                             | 1.044          | n/a          |
| payload keyed by the block, not by a surrogate | 1.147          | 0.75 ms      |
| the same, without the expiry index             | 1.046          | 0.80 ms      |
| without either maintenance index               | **0.964**      | 0.74 ms      |
| body inside the block row                      | 1.094          | 2.13 ms      |
| the same, without the expiry index             | 0.993          | 2.16 ms      |
| key and body only                              | 0.833          | n/a          |

The 275.3 bytes a block of the first row divide like this:

| object                                           | bytes a block | bytes a sample |
|--------------------------------------------------|---------------|----------------|
| `payloads`, of which 160.8 is the payload itself | 176.5         | 0.736          |
| `blocks`                                         | 54.5          | 0.227          |
| `block_expiry`, over `end_ts`                    | 24.2          | 0.101          |
| `block_payload`, over the foreign key            | 19.7          | 0.082          |
| schema                                           | 0.4           | 0.002          |

Three results, each against an expectation.

**SQLite already stores an integral value in a `real` column as an integer, in
a `STRICT` table too.** Declaring the summary columns `integer` changed nothing,
byte for byte. What the summary really costs is the difference against dropping
it, and that depends on the data rather than on the declaration —
`TestWhatASummaryCostsWhenItIsNotWholeNumbers`, same fixture:

| values             | `blocks` b-tree | the summary's share |
|--------------------|-----------------|---------------------|
| whole numbers      | 54.5 B/block    | 24.6                |
| tenths of a degree | 60.6            | 30.7                |
| noisy `float64`    | 91.3            | 61.4                |

Eight bytes a column when the value is not integral, which is 0.256 bytes a
sample for a noisy series.

**Giving the payload the block's own key saves nothing.** It removes the
19.7-byte foreign-key index and 3.3 bytes of `payload_id`, and the composite
key it puts in the payload table costs 23. The earlier 0.051 came from a
different schema — a rowid block table with a unique index — and does not
generalise.

**The body inside the block row is worse on both counts.** It is larger
(0.993 against 0.964) and its summary scan is three times slower, and the page
counts say why rather than leaving it to a story: the block b-tree grows from
54.5 to 238 bytes a block, so a scan that wants eleven columns turns four times
as many pages.

What reaches 0.964 is neither a codec nor a clustering change: it is dropping
two maintenance indexes, and the two are not equally free. `block_payload`
exists only so that deleting a payload can find the block pointing at it, which
two deletes in one transaction do for nothing, so `1.046` is available against
one gate — that no payload outlives its block. `block_expiry` finds expired
blocks across the whole file, and removing it is a claim about a retention pass
that walks series instead, which is not measured here. Until it is, `0.964` is
a number with a debt against it and `1.046` is the one to quote.

## A million series

The design target is that the format and the query model hold about a million
active series without being redesigned, which is not the same as letting an
embedded installation have a million. Two shapes of the same million, generated
as telemetry would really send them — fifty thousand installations, twenty
series each, one label of enormous cardinality beside three small ones —
measured in a `golang:1.27` container on linux/amd64:

|                                     | in memory | in SQLite |
|-------------------------------------|-----------|-----------|
| register a million series           | 461 ms    | 8.51 s    |
| index 3.25M postings                | included  | 5.51 s    |
| heap the registry holds             | 171.2 MiB | 1.8 MiB   |
| file                                | —         | 134.3 MiB |
| resolve one series, the ingest path | 157 ns    | 4.51 µs   |
| select `kind=mysql`, 62 500 series  | 2 µs      | 8.86 ms   |
| intersect two labels, 33 340        | 7.28 ms   | 41.6 ms   |
| select one installation, 20 series  | 2 µs      | 111 µs    |
| open cold and count                 | —         | 17 ms     |
| select after that cold open         | —         | 102 µs    |

The file splits 91.1 MiB of strings and series against 43.3 MiB of postings.
RSS is not in the table because the million generated series dominate it in
both runs; it measures the harness, not the registry.

The row that decides the shape is not in the table at all: **loading every
posting list back into memory costs 961 ms and 20.4 MiB**. Postings are cheap
to hold and the fingerprint index is what costs — 171.2 minus those 20.4 is a
map from a label set to a series id, and it is the part that does not fit a
small container.

So the split follows the measurement rather than taste. SQLite holds the truth,
which is what the tables in [tinystore.md](tinystore.md) already said and what
a 17 ms cold open makes free. Postings are held in memory, because 20 MiB buys
a selector four thousand times faster. Resolving a label set to a series id is
a bounded cache over the file, because at an embedded twenty thousand series it
holds everything and at a million it holds the working set.

What that leaves is one number per workload rather than one promise:

```text
telemetry, 11 samples/s    4.51 µs uncached is 0.005% of a core
a 15s scrape, 66k/s        4.51 µs uncached is 30% of a core, so the cache is
                           what makes that rate possible at all
```

The harness is `spike/`, skipped unless `TINYSTORE_SPIKE` is set, so
re-measuring costs one command rather than an argument.

## A block, a head and a reader at the same time

The second spike packs what arrives, and runs a writer, a compactor and a
reader against one file at once. Same container, linux/amd64.

### What a block costs at each size

One synthetic gauge, 30 720 samples, cut into blocks of different sizes. The
payload is varint timestamps, raw float bits and zstd; the overhead is what
SQLite adds on top of it.

| samples per block | on disk per sample | payload per sample | overhead |
|-------------------|--------------------|--------------------|----------|
| 60                | 12.00 B            | 8.54 B             | 41%      |
| 120               | 9.87 B             | 7.78 B             | 27%      |
| 240               | 9.60 B             | 7.44 B             | 29%      |
| 480               | 9.47 B             | 7.33 B             | 29%      |
| 960               | 9.20 B             | 7.28 B             | 26%      |
| 1920              | 8.13 B             | 7.26 B             | 12%      |

Small blocks are expensive and large ones are not: the curve falls all the way
to 1920 samples. That contradicts the four-hour row in the table above, which
measured 86% against an hour's 16%, and the contradiction is not resolved — the
two harnesses differ in what they store, so what this says is that **the byte
ceiling is not protecting the disk**. It protects what a query pays: a block is
decompressed whole, so one point out of a 1920-sample block costs all 1920.

The payload here is 7.3 B a sample against the 2.61 B the codec table reports
for a noisy gauge, and the difference is the data rather than the code. This
generator walks a `float64` by a random step, so every mantissa bit is noise
and `zstd` has nothing to find. A real gauge — bytes, a count, a percentage
with one decimal — carries far fewer significant bits and compresses the way
the codec table says. **What a sample costs therefore depends on how many
digits the number really has**, which is a property of the workload and not of
the format, and neither number is wrong. What is missing is the realistic
middle, and that is a measurement rather than an argument.

### A writer, a compactor and a reader on one file

Ten seconds, 200 series, blocks closed at 240 samples, a reader taking
snapshots in a loop the whole time:

```text
wrote              303 800 samples in 10.0s        30 364 samples/s
packed             192 000 samples into 800 blocks
still in the head  111 800 samples
reads               71 202 snapshots
file                 6.0 MiB
```

The head keeps more than it packs because the compactor seals only what the
watermark allows; the tail is what something late is still permitted to land
in. The log is left out of this run on purpose — its reader never pauses, which
is a load generator's habit and not a panel's, and the run below is where the
log is actually measured.

The invariant held on every one of those 71 202 reads: what one snapshot holds
— packed plus head — equalled what the same snapshot said had been written,
exactly, and the totals agreed at the end. Writing the block and deleting what
it packed in one transaction, and reading both sides in one read transaction,
is enough; nothing else was needed.

The last number wanted care rather than a conclusion, and a second run took it
apart. Forty seconds, the same writer and compactor, read the way a dashboard
reads — one reader a second, one every five, one holding half a second — and at
ten seconds a reader that holds a single snapshot open for twenty:

```text
 0–8s    4.0 → 7.7 MiB     the ordinary readers; the log barely moves
10s      9.1 MiB           the long reader arrives
12–28s   33 → 202 MiB      about 10 MiB a second, while the snapshot is held
30s+     221.1 MiB, flat   it left; writing continues and the file stops growing

passive checkpoint   419 frames in the log, 419 of them copied
the file after it    221.1 MiB
truncate             the log is 0.0 MiB and the database is 2.0 MiB
```

Two hundred and twenty megabytes of file held under two megabytes of live log. The
file size and the log's contents are different facts, and the first one alone
proves nothing — which is what the earlier run's 109 MiB was, a file size
mistaken for a diagnosis.

What governs the log is **the oldest open read transaction**, not how often
anybody reads. Three readers behaving like panels left it at 4 to 9 MiB; one
reader holding a snapshot for twenty seconds took it to two hundred. The moment
that reader left, growth stopped, because a checkpoint could advance again and
the space inside the file was reused.

Latency did not notice any of it:

```text
a reader every second      p50 293 µs, p95 546 µs, p99 562 µs, worst 647 µs
a reader every five        p50 431 µs, p95 495 µs, p99 495 µs
a write round              p50 1.43 ms, p95 10.17 ms, p99 12.69 ms, worst 42.6 ms
```

So the rule this store needs is about duration rather than frequency: a read
transaction gets a deadline, the log gets a size limit, and giving the file
back needs a `truncate` checkpoint at a moment with nobody reading. A panel
refreshing every second is not the thing to defend against; an export, a stuck
query or a paused debugger holding one snapshot is.

At 30 000 samples a second through one writer connection with a hundred and
fifty transactions a second, the embedded target of a few thousand has room;
the 66 000 a second that a million fifteen-second series would need is within
reach of the same path and has not been measured.

### What a query pays for a block size

The disk stopped arguing for a ceiling, so the ceiling had to be argued for by
the reader. A block is decompressed whole, so a panel asking for an hour of a
fifteen-second series — 240 samples — pays for whatever the block around it
holds:

| samples per block | payload | decode  | per sample | held     | decoded for 240 wanted |
|-------------------|---------|---------|------------|----------|------------------------|
| 60                | 509 B   | 2.8 µs  | 47.1 ns    | 2.0 KiB  | 240, over four blocks  |
| **240**           | 1774 B  | 6.4 µs  | 26.5 ns    | 6.7 KiB  | **240, in one**        |
| 480               | 3516 B  | 12.9 µs | 26.9 ns    | 13.5 KiB | 480                    |
| 960               | 6968 B  | 23.7 µs | 24.6 ns    | 27.0 KiB | 960                    |
| 1920              | 13879 B | 37.2 µs | 19.4 ns    | 54.0 KiB | 1920                   |

Below the window the per-sample cost doubles and the disk overhead was 41%;
above it every sample beyond the window is decompressed and held for nothing —
eight times over at 1920. The two costs meet where the block matches the window
a panel asks for, which is 240 samples and about 1.7 KiB, and that is where the
thresholds are set:

```text
targetSamples   240
maxBytes        2 KiB
```

An hour of one series then costs 6.4 µs and 6.7 KiB, so a panel over twenty
series holds 134 KiB while it draws. The hour in the original table was right;
what was wrong was the reason given for it, which was the disk.

### What a sample costs, and what that depends on

The codec table reports 2.61 bytes for a noisy gauge and 1.96 for a counter.
Measuring four shapes at 240 samples a block says the axis is not the kind of
metric at all:

| the values are         | bytes per sample |
|------------------------|------------------|
| a constant             | 0.18             |
| whole numbers          | 1.25             |
| a noisy `float64` walk | 7.43             |
| a counter of `float64` | 8.13             |

A counter coming out worst than a gauge is the giveaway. Both of the expensive
rows are full-entropy `float64`: every mantissa bit is noise, and `zstd` has
nothing to find. The cheap rows are numbers with few significant digits — which
is what bytes, request counts, connection totals and a percentage with one
decimal actually are.

So the honest sentence is a range with its reason rather than a number:
**between about 0.2 and 8 bytes a sample, decided by how many significant
digits the values carry**. Integer-like operational metrics measured 1.25
bytes; real metrics also include stable and noisy floating-point values. The
codec table's 1.96 for a counter was measured on integers; nothing disagrees,
the two were counting different data.

### What deleting raw would make a query forget

A summary for an adaptive block cannot answer a range that cuts the block in
the middle. Five-minute buckets only move that boundary error from the whole
block to five minutes. So the storage saving was measured against the semantic
cost rather than treated as a free retention step.

Thirty days of fifteen-second series, stored as block rows and payload blobs in
SQLite, projected to 30 million samples:

| values          | raw for 30d | 7d raw + 23d five-minute | summaries only |
|-----------------|-------------|--------------------------|----------------|
| whole numbers   | 45.6 MiB    | 37.4 MiB                 | 6.2 MiB        |
| noisy `float64` | 255.8 MiB   | 130.7 MiB                | 10.9 MiB       |

These are blocks and their blobs, not the whole `metrics.db`: the series
registry, live head and WAL are outside the measurement. The integer-like tier
saves only 8.2 MiB over a million samples a day, while making arbitrary range
edges inexact.

Nor is a rollup automatically smaller. The same five-minute encoding over
whole-number values costs 66% of raw at a fifteen-second interval, 155% at one
minute, 393% at five minutes and 174% at one day. A sparse series would pay more
to remember less.

The first version therefore keeps raw for the lifetime of its block. A query
uses the summary for whole blocks and decodes blocks cut by its boundaries, so
the answer stays exact. A later coarse tier may trade that property for space,
but its effective resolution has to be visible to the caller, and a sparse
block keeps raw when its rollup would be larger.

### Capacity after raw and block got the same lifetime

The density measurement held the sample count at 2.4 million and changed only
the interval. At 240 samples per block, interval does not affect storage; at a
daily interval the thirty-day span closes a block at thirty samples and row
overhead becomes visible:

| values                       | 15s / 1m / 5m / 1h | 1d             |
|------------------------------|--------------------|----------------|
| whole numbers, payload       | 1.23 B/sample      | 3.57 B/sample  |
| whole numbers, full SQLite   | 1.55 B/sample      | 5.74 B/sample  |
| noisy `float64`, payload     | 7.41–7.42 B/sample | 9.13 B/sample  |
| noisy `float64`, full SQLite | 8.94 B/sample      | 12.99 B/sample |

Here full SQLite means blocks, payloads, summaries, `series_state` and page
overhead, not the registry, live head or WAL. At fifteen seconds and thirty
days that projects to 511 MiB or 2.88 GiB for 2 000 series, and 2.49 GiB or
14.39 GiB for 10 000, depending on value entropy. The durable head measured
18.72 bytes per row; reserving one open 240-sample block per series plus the
registry moves the physical envelopes to about 520 MiB–2.89 GiB and
2.54–14.43 GiB respectively.

The earlier sparse numbers were component fixtures: 311.1 MiB for forced
monthly blocks plus registry, 843.3 MiB after adding a separately measured head,
and 2.51 GiB for forced singleton blocks. The 843.3 MiB figure was incorrectly
called a measured physical high-water. Those fixtures did not simulate live
sparse expiry and used zero-valued frontiers; they are not capacity guarantees.

The corrected design leaves a quiet tail in the durable head. Query and ingest
both enforce the retention cutoff independently of the sealed frontier. GC
deletes expired head rows directly and wholly expired blocks. A boundary block
can physically contain expired samples while every query clips them out before
aggregation. No rewrite and no one-hour forced sealing are needed.

Ninety-day daily-head simulations at 20 000 and 100 000 series, with populated
frontiers, identifiers spread up to one million and a per-series expiry index,
plateaued at 12.695 and 63.488 MiB respectively. Each holds 31 samples per series
immediately after arrival because cutoff equality is retained. Both produced
zero blocks: expiry overtook the packing threshold. Projections to one million
series plus the prior 134.3 MiB registry are 769.1 and 769.2 MiB. This excludes
WAL and remains a workload projection, not a million-series whole-process run.

The ninety-day churn run did settle. With thirty live days, whole-number data
went from a 32.7 MiB live file on day 30 to a 33.9 MiB physical file on day 90,
holding 0.9 MiB free. Noisy floats went from 188.8 MiB to 195.3 MiB, holding 6.1
MiB free. SQLite keeps the pages and reuses them; it does not keep growing.

### Which simplifications actually saved resources

The block layout comparison writes 20 000 blocks per case with matching samples,
correct payload timestamps, changing series values, populated state, enabled
foreign keys and indexes for block expiry and payload references. It measures
the physical main file after checkpoint, including page overhead:

| workload                 | separate payload B/sample | inline rowid | inline without rowid |
|--------------------------|---------------------------|--------------|----------------------|
| 240 integer-like samples | 1.858                     | 1.807        | 1.958                |
| 240 noisy floats         | 9.132                     | 8.708        | 19.565               |
| 1 daily sample           | 129.229                   | 108.134      | 101.990              |
| 7 daily samples          | 22.499                    | 19.573       | 18.900               |
| 14 daily samples         | 12.961                    | 11.469       | 11.396               |
| 30 daily samples         | 7.011                     | 6.335        | 6.485                |

Summary scans of 20 000 dense blocks took 2.45/3.00 ms for separate integer/float
payloads, versus 5.43/14.85 ms for inline rowid. These are warm local timings,
not throughput promises. Inline storage saves little on dense data and slows
summary reads; inline `without rowid` is especially expensive for noisy blobs.
Keep summaries and payloads separate. The new figures include maintenance
indexes missing from the earlier 1.55/8.94-byte fixture.

Packing sparse data earlier also did not win on this workload. A 20 000-series,
ninety-day lifecycle with safe-prefix packing, enabled foreign keys and the same
maintenance indexes logged these physical peaks:

| packing span              | samples per packed block | measured peak | 1M projection + registry |
|---------------------------|--------------------------|---------------|--------------------------|
| 7 days                    | 8                        | 16.188 MiB    | 943.7 MiB                |
| 14 days                   | 15                       | 13.801 MiB    | 824.3 MiB                |
| 30 days, head expiry wins | no blocks                | 12.695 MiB    | 769.1 MiB                |

The first two rows are the completed case results from `TestSparsePackingSpans`;
the third is the independently completed `TestSparseHeadLifecycle`. Stored
samples were decoded and compared with expected values throughout the packing
cases. Retention is logically exact in each case. A shorter span is therefore
not a default disk optimisation for these integer-like daily series.

The largest measured RAM saving was in codec configuration. At GOMAXPROCS=16,
after warm-up and GC, the live Go heap added by an encoder/decoder pair was:

| settings                                       | live heap delta | encode / decode per 240-sample block |
|------------------------------------------------|-----------------|--------------------------------------|
| defaults                                       | 25.451 MiB      | 17.19 / 7.59 microseconds            |
| one encoder and decoder worker                 | 1.601 MiB       | 16.25 / 6.52 microseconds            |
| one worker, 8 KiB window, lower encoder memory | 1.338 MiB       | 14.28 / 6.96 microseconds            |

Each setting produced 1779-byte float and 296-byte integer payloads and passed
the exact round trip. Timings average 2 000 sequential calls; this does not
measure parallel throughput or total process RSS. One shared writer codec and a
bounded reader pool are appropriate for the single-writer design. The bounded
8 KiB setting must agree with the production format's maximum decoded size;
the spike alone does not establish compatibility with every future block.
