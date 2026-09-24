# The design

How the metrics store is meant to work, and why it has this shape rather than
another. The runtime and the other engines are in [architecture.md](architecture.md).
The first durable engine now lives in `metrics/`; [its README](../metrics/README.md)
states implemented behavior. This document preserves the design and experiments,
including proposed features and older layouts; it is not an API reference.
The numbers are quoted here where they decided something and recorded in full,
with their environment and their reproduction, in
[measurements.md](measurements.md). What is still open is in
[research.md](research.md), and the bytes are in [format.md](format.md).

## What it is, and what it refuses to be

A bounded local store for numbers that move over time: a durable head of recent
samples and immutable encoded blocks behind it, in one SQLite file. It is
deliberately useless above that. A stream of every HTTP request, every log line
or every span belongs in something built for it, and so does anything that
wants a cluster.

The boundary is what keeps it small. Each time the design met a workload it
could not serve, the answer was that somebody else should serve it, and that is
why everything else stayed simple. A caller that also needs to record discrete
facts — what happened, and how often — wants a table per kind of fact and not
this.

## Why not the Prometheus TSDB

It is the obvious candidate and it was measured before being refused.

|                                          | size       | method pruning |
|------------------------------------------|------------|----------------|
| the binary that will embed this          | 18 044 KiB | intact         |
| the same, plus `prometheus/tsdb`         | 26 620 KiB | **off**        |
| hello world + `prometheus/config`        | 14 624 KiB | **off**        |
| hello world + `client_golang`            | 8 444 KiB  | **off**        |
| hello world + `prometheus/tsdb/chunkenc` | 3 800 KiB  | intact         |

The +8.4 MiB is bad; the second column is worse. `tsdb` imports
`prometheus/config`, which reaches `common/config`, `go-conntrack`,
`x/net/trace` and finally `text/template`, whose `evalField` calls
`reflect.Value.MethodByName` with a name it computes at run time. That switches
off the linker's method pruning for the whole binary — the tax `task size`
already prices at +3.4 MiB. No test catches it yet; `task size` shows the
growth.

A trimmed fork is possible: `prometheus/storage` on its own pulls none of that,
so cutting the `config` import is an evening. Cutting `client_golang` is not —
it means replacing the TSDB's internal metric registration throughout a package
whose upstream pushes daily. That is a subscription, not a patch.

## Why the first codec was generic

Also measured rather than assumed. Thirty days at fifteen seconds, hourly
blocks, bytes per sample:

|                        | noisy gauge | counter  | stable gauge | pack  | unpack | binary     |
|------------------------|-------------|----------|--------------|-------|--------|------------|
| raw binary             | 16.00       | 16.00    | 16.00        | 2 µs  | —      | +0         |
| raw + gzip             | 7.24        | 6.53     | 4.95         | 73 µs | 16 µs  | +0         |
| raw + zstd             | 6.47        | 5.53     | 2.62         | 56 µs | ~0     | +688 KiB   |
| **varint time + zstd** | **2.61**    | **1.96** | **0.12**     | 34 µs | 6 µs   | +688 KiB   |
| gorilla (`chunkenc`)   | 6.34        | 1.94     | 0.32         | 7 µs  | 3 µs   | +2 248 KiB |
| gorilla + zstd         | 3.15        | 2.00     | 0.14         | 44 µs | 4 µs   | +2 936 KiB |

Varint deltas on the timestamp, raw float bits for the value, zstd over the
whole block: 2.4 times smaller than Gorilla on a noisy gauge, level on a
counter, better on a stable one, and a third of the binary cost. XOR looks at
the neighbouring sample; zstd looks at the block, and on noisy data that wins.

Stacking them is worse than either: `gorilla + zstd` costs 3.15 against 2.61,
because the XOR layer destroys exactly the patterns the compressor was going to
find.

That was the argument for one generic encoding, and it was right about Gorilla
and wrong about the conclusion. What it compared was one fixed transform
against another; what the codec does now is look at the samples and choose.
Reading the data first is what the generic path could not do, and it halved
integers: whole numbers went from 1.266 to 0.671 bytes a sample on identical
input, a counter from 1.424 to 0.808. On a noisy float it is still nearly the
same number, which is the honest half of the result — the entropy is in the
value and no lossless encoding makes it go away. [format.md](format.md) says
what the codec writes; [measurements.md](measurements.md) has every workload.

## SQLite is the substrate

Not as a time-series database — as a transactional, indexed container for our
own immutable compressed blocks, in a file of its own.

```text
metrics.db     series, postings, the head, blocks and payloads
```

| ours                               | SQLite's                           |
|------------------------------------|------------------------------------|
| block encoding, cutting scrapes up | write-ahead log and crash recovery |
| the resolution planner             | transactions                       |
| dimension dictionaries             | series and posting indexes         |
| cardinality guards                 | the block catalogue                |
| retention policy                   | retention as one statement         |
|                                    | backup through `vacuum into`       |

That trade deletes the parts of a storage engine that are genuinely dangerous
to write: the log, the segment format, the offset index, the atomicity between
a block and its index entry, the file juggling a retention pass would do.

It costs a file of its own, and that is deliberate: metrics have their own
retention, their own write load and their own right to break alone, and a
corrupt metric store must never stop the application that embeds it from
starting. Whatever else the caller keeps in SQLite, it keeps somewhere else.

## How a series is stored

### An hour is what a fifteen-second series produces, not a constant

A schema and page-size grid, 100 series over 30 days, size after a checkpoint:

|                              | hourly blocks | four-hour blocks |
|------------------------------|---------------|------------------|
| one table, `without rowid`   | +22%          | +115%            |
| one table, rowid             | +18%          | +87%             |
| meta + payload table         | +18%          | +86%             |
| **clustered meta + payload** | **+16%**      | +86%             |

Page size — 4K, 8K, 16K — changed nothing in any row. What decides is the block
size: an hour of a fifteen-second series compresses to roughly 620 bytes, four
hours is 2.5 KiB, and the overhead goes from 16% to 86%. The layout moves the
result by percents; the block size moves it fivefold.

Why 2.5 KiB is where it turns is **not** established. The obvious explanation
is a payload spilling out of its page, and the row above refuses it: if that
were the mechanism, a 16K page would have moved the number and it did not. So
the economics are measured and the cause is not, which is the difference
between a threshold to aim at and a rule to build on.

The winner keeps small rows where `without rowid` belongs and blobs where it
does not:

```sql
create table blocks (
    series_id  integer not null,
    start_ts   integer not null,
    end_ts     integer not null,
    count      integer not null,
    min real, max real, sum real,
    first real, last real,
    increase real,
    resets integer not null,
    payload_id integer references payloads(id) on delete set null,
    primary key (series_id, start_ts)
) strict, without rowid;

create table payloads (id integer primary key, body blob not null) strict;
```

`end_ts` is a column because the span is not a constant, which the next section
is about. The rest is what a query over the whole block can answer without
decoding its raw samples, and half of it is per kind: `sum`, `min` and `max`
mean something for a gauge, `increase` and `resets` for a counter.

Clustering on `(series_id, start_ts)` is also why its summary query took 4 ms
where the others took 8 to 25: a month of one series is a sequential read.

`payload_id` leaves room for a future coarse retention tier, and the foreign key
does its bookkeeping — deleting a payload nulls the reference instead of
leaving an orphan. The first version does not use that room: its raw payload
lives for as long as its block, because the section below measures what deleting
it would make a query forget.

### A counter keeps its increase, or the number is destroyed with the raw samples

`first` and `last` are not enough, and no amount of care at query time recovers
what the summary did not keep:

```text
samples   100 → 110 → 5 → 20     a reset between 110 and 5
truth     +10, then +5, then +15, so the increase is 30
summary   first 100, last 20, resets 1
```

Thirty is unreachable from that row: it needs the value the counter stood at
when it reset, which is 110. So the compactor computes the increase while it
has the samples in order and stores it; a full-block query should not decode a
payload to recover a statistic the block already owns. `resets` stays as
diagnosis — somebody restarted — and not as a term in the arithmetic.

Across blocks the same question is answered by comparing the next block's
`first` against the previous one's `last`, which is why all three are columns.

A reset is only visible in time order, and this store accepts samples at any
timestamp: an agent that buffered a day and sent it at once arrives out of
order. The compactor therefore sorts what it is packing rather than trusting
the order it was written in.

### The span is a consequence, and nothing computes an average

A block closes on whichever comes first:

```text
enough samples      a bounded worst case for one row
enough bytes        so that a whole number of rows fills a page, about 1300
too wide a span     limits the event-time range of one block
```

The period falls out of that by itself:

```text
every 15 seconds   240 samples arrive in an hour        closed by count
every minute       240 samples arrive in four hours     closed by count
once a day         about 30 retained samples            may stay in the head
```

What this deliberately does not do is measure a series' average interval and
pick a period from it. An average is a prediction, and a fortnight of downtime
makes it predict nonsense; "enough has arrived" survives a gap without noticing
it. The byte condition is checked every so many samples rather than after each
one, because the encoded size is only known by encoding. Packing thresholds
apply to the unexpired safe prefix. `maxBlockSpan` caps a block's event-time
coverage; it is not an idle timer that creates a singleton whenever a series
stops. A sparse series can expire from the head without ever producing a block.

### Ingest lands somewhere durable, and a query sees one moment

The head is a table, not a buffer in memory: twenty minutes of collected
samples that vanish with the process is the loss nobody notices until it
matters.

```text
ingest → head (durable) → compactor → blocks and payloads
```

A read transaction covers fetching the bytes and nothing after it. The blobs
and head rows come out of one snapshot, which makes them a consistent set that
stays consistent once the transaction ends, so decoding, merging and building a
frame all happen outside it. Otherwise one heavy panel becomes the long reader
the log measurement warns about — and this is the other reason a block has a
size ceiling, because those payloads are held in memory for as long as the
query runs.

Two invariants hold it together. The block is written and the head rows it
packed are deleted **in one transaction**, and deleted by their own keys rather
than by a range, or a sample that arrived late inside that range is deleted
without ever reaching a block. And a query reads blocks and head **inside one
read transaction**, so it sees the state before a compaction or the state after
it, never half of each — without that, a compaction running beside a reader
shows a gap or a doubled value depending on which half was read first.

They were also enough: a writer, a compactor and a reader ran against one file
for ten seconds, and what a snapshot held equalled what it said had been
written on every one of 71 202 reads.

### Lateness is bounded, because a late counter sample rewrites a sealed block

Deleting from the head by key rather than by range saves a late sample from
being thrown away. It does not save the summary that was already written
without it, and for a counter that is not an approximation:

```text
sealed   100 at 10:00, 110 at 10:10, 120 at 10:20      increase 20, no resets
arrives  5 at 10:15, because the counter restarted
truth    100 → 110 → 5 → 120                           increase 130, one reset
```

Twenty against a hundred and thirty, and packing the late one into a block of
its own does not help: two blocks that overlap in time cannot have their
increases added. So what may be sealed is bounded by a watermark, and the
watermark trails **the newest sample that series has shown**, not the clock:

```text
newest seen for this series   14:30
allowed lateness               5 min
sealable                      strictly before 14:25
still open                    14:25 and after
```

Following the series rather than the clock is what keeps an agent that was away
for a day from being late merely because the server moved on while it was gone:
its backlog arrives, carries its own timestamps, and moves its own watermark.

The watermark is a candidate, not the contract. What a caller may rely on is a
frontier that is **kept**, per series, and never moves backwards:

```text
max_seen_ts     the newest timestamp this series has ever shown
watermark       max_seen_ts − allowed_lateness, what is safe to seal now
sealed_before   what has been sealed and can no longer change
```

Ingest checks `sealed_before` and the current retention cutoff: a sample below
either is refused and counted. An unsealed sample inside retention may still
arrive; the candidate watermark is not another rejection rule. Only sealing
moves `sealed_before`, inside the transaction that wrote the block. A store
that recomputed a watermark from whatever happened to be in the head after a
restart would have forgotten what it had already promised not to rewrite.

And the compactor packs **the safe prefix**, not what it happened to select:

```text
240 in the head, the watermark allows 217
→ seal 217, leave 23
```

A block builder that takes its limit and packs it has an out-of-order window in
the document and none in the code, which is the shape this kind of bug always
has.

The head is already durable and queryable. A series that stops moving its
watermark therefore needs no forced closure: its remaining samples can stay in
the head until they expire. The two clocks answer different questions:

```text
allowed_lateness   event time     what may be sealed without losing the order
retention cutoff   wall clock     what may still be accepted and read
```

There is no `max_open_age = 1h` rule. Turning a quiet tail into a block buys no
durability and can turn thirty daily samples into thirty payload rows. Retention
deletes expired head rows directly; it need not manufacture a block to delete
them. The oldest head timestamp is maintained per series and indexed for due
work, followed by a primary-key range delete within that series. The expiry
index therefore has one entry per nonempty head, not one per sample, and the
sweeper uses bounded batches rather than scanning every series.

Which of the two moves, and who moves it, is the whole contract, so it is
written out rather than implied:

```text
ingest      refuses anything before cutoff or sealed_before, heads the rest,
            and moves max_seen_ts once at the end of the batch.
            it never touches sealed_before
compact     takes the watermark, seals the safe prefix, deletes exactly what it
            sealed and moves sealed_before — all in one transaction
retention   expires head rows directly and deletes wholly expired blocks;
            it does not advance sealed_before
```

Ingest never promises that the past is closed, because it is not the one that
closed it. And a backlog is one batch: an agent offline for an hour sends what
it accumulated and `max_seen_ts` moves once at the end, where moving it sample
by sample would have the batch start refusing its own neighbours halfway
through.

Bounded lateness is the live path's rule and not the format's. Loading a year
of history would deserve the answer "too late" from ingest and would be useless
for it, so the format leaves the other door open: blocks may be sorted and
built directly, without passing through the head or being judged by a live
`sealed_before`. Nothing of that is built, and what matters now is only that
nothing here forbids it.

A sample behind the sealed frontier is refused rather than reopening a block.
Retention independently refuses expired samples even when the head is empty
and no block was ever sealed. **A refused sample is counted and the count is
visible**, or the first operator with a stuttering agent spends a week
wondering about the gaps.

### Raw lasts as long as the block in the first version

An adaptive block summary cannot answer a range that cuts through it. Given a
block from 10:00 to 11:00, its `min`, `max`, `sum` and `increase` say nothing
exact about 10:17 to 10:43. Five-minute buckets narrow the lie but do not remove
it: the two edge buckets still contain 10:15–10:17 and 10:43–10:45.

The first version preserves raw alongside every retained block:

```text
head row before cutoff           delete directly
block entirely before cutoff     delete block and payload
block crossing cutoff            keep payload, clip every read
```

A query captures one cutoff and uses `[max(from, cutoff), to)` for head and
blocks from one snapshot. Columns can answer a block only when every sample
belongs to that range and the same output bucket. A block cut by either range
edge or a bucket boundary is decoded and filtered before aggregation, including
counter increase. The observed counter increase uses samples in that interval;
it neither imports an expired predecessor nor promises PromQL extrapolation.
Removing the payload earlier would require the API
to expose a coarser effective resolution or to admit that the answer is
approximate; silently snapping the range is not a storage optimisation.

The cost was measured over 30 days of fifteen-second series in the actual
blocks-and-payloads SQLite layout. The projection is 30 million samples — one
million a day — and excludes the series registry, the live head and the WAL, so
it is not called the size of `metrics.db`:

| values          | raw for 30d | 7d raw + 23d five-minute | saving    |
|-----------------|-------------|--------------------------|-----------|
| whole numbers   | 45.6 MiB    | 37.4 MiB                 | 8.2 MiB   |
| noisy `float64` | 255.8 MiB   | 130.7 MiB                | 125.1 MiB |

The tier is not even smaller for every density. With whole-number values, the
five-minute blob costs this much against raw:

| sample interval | five-minute / raw |
|-----------------|-------------------|
| 15 seconds      | 66%               |
| 1 minute        | 155%              |
| 5 minutes       | 393%              |
| 1 day           | 174%              |

So a later coarse tier is allowed, but it is a query-semantics feature rather
than part of block deletion. It must expose its resolution, and a sparse block
keeps raw when the rollup would be larger. Neither condition belongs in the
first store slice.

### Capacity depends on density as well as value entropy

The same 2.4 million samples were written at five intervals. `sqlite` includes
the block catalogue, payload rows, summaries, `series_state` and page overhead;
it excludes the registry, live head, maintenance indexes and WAL. These are
prepacked storage fixtures, not a live ingest/retention simulation: the first
four densities use 240 samples, while the daily case forces thirty samples per
block. Repeated blobs and zeroed series frontiers make these component estimates,
not measured production capacity. In particular, live daily ingestion need not
produce the forced monthly blocks in this table:

| values          | interval | samples/block | blocks | payload B/sample | SQLite B/sample |
|-----------------|----------|---------------|--------|------------------|-----------------|
| whole numbers   | 15s      | 240           | 10 000 | 1.23             | 1.55            |
| whole numbers   | 1m       | 240           | 10 000 | 1.23             | 1.55            |
| whole numbers   | 5m       | 240           | 10 000 | 1.23             | 1.55            |
| whole numbers   | 1h       | 240           | 10 000 | 1.24             | 1.55            |
| whole numbers   | 1d       | 30            | 80 000 | 3.57             | 5.74            |
| noisy `float64` | 15s      | 240           | 10 000 | 7.41             | 8.94            |
| noisy `float64` | 1m       | 240           | 10 000 | 7.42             | 8.94            |
| noisy `float64` | 5m       | 240           | 10 000 | 7.41             | 8.94            |
| noisy `float64` | 1h       | 240           | 10 000 | 7.42             | 8.94            |
| noisy `float64` | 1d       | 30            | 80 000 | 9.13             | 12.99           |

A thirty-day, fifteen-second deployment therefore projects to:

| series | samples       | whole numbers | noisy `float64` |
|--------|---------------|---------------|-----------------|
| 2 000  | 345.6 million | 511 MiB       | 2.88 GiB        |
| 10 000 | 1.728 billion | 2.49 GiB      | 14.39 GiB       |

The registry adds about 0.3 MiB and 1.3 MiB respectively at its measured 134.3
MiB per million series, which is noise beside those payloads. These are
projections from measured bytes per sample, not files that held billions of
samples. The old 18.72-byte head-row fixture suggested another 8.6 MiB for 2 000
series and 42.9 MiB for 10 000 if each holds 240 pending samples. That is not an
upper bound: the unsealable lateness window, compaction backlog, value width,
maintenance indexes and allocator high-water also matter.

The previous sparse projections are retained as component experiments in
`capacity_spike_test.go`: 311.1 MiB for forced monthly blocks plus registry,
843.3 MiB after adding a separately measured growing head, and 2.51 GiB for
forced singleton blocks. None ran sparse ingest, expiry and packing together.
In particular, 843.3 MiB was not a measured physical high-water and must not be
used as a capacity guarantee. The one-hour forcing rule has been removed.

### A daily series can live entirely in the head

With thirty-day retention and a thirty-day maximum block span, a daily series
need never pack. After discarding expired rows, its safe prefix stays below 240
samples, 2 KiB and thirty days of span. Expiry can therefore overtake the packing
threshold. This is a supported path: raw rows are durable, indexed by series
and time, and queryable throughout their lifetime.

`TestSparseHeadLifecycle` runs that path for ninety days at two scales. Each day
it deletes expired head rows using the per-series expiry index in 1 000-series
transactions, ingests one integer-valued sample per series, updates the real
timestamp frontiers, checks packing eligibility and checkpoints. IDs span the
same 1..1 000 000 range at both scales. Samples exactly at the cutoff survive,
so immediately after a daily arrival the thirty-day interval holds 31 samples.

| series  | head rows at plateau | day 30 file | day 31 file | day 90 file |
|---------|----------------------|-------------|-------------|-------------|
| 20 000  | 620 000              | 12.324 MiB  | 12.688 MiB  | 12.695 MiB  |
| 100 000 | 3 100 000            | 61.496 MiB  | 63.480 MiB  | 63.488 MiB  |

Both runs produced zero blocks. The file includes the head, populated
`series_state`, the `oldest_head_ts` expiry index and SQLite page overhead. At
the larger scale it projects to 634.9 MiB for a million series; adding the prior
134.3 MiB registry measurement gives **769.2 MiB**. The smaller run projects to
769.1 MiB, which is a useful scale check, not a million-series end-to-end test.
The result excludes WAL, a compaction backlog, noisy floating-point head values
and changes to the registry's label distribution. It is a measured integer-like
workload projection, not a universal sub-gigabyte promise or a RAM measurement.

### Logical expiry and whole-block reclamation have different boundaries

With `delete where end_ts < cutoff`, an expired sample can remain in an
overlapping block until retention plus almost one block span and the next sweep.
Every query still filters at its captured cutoff. The following lifetimes
describe membership in a stored block, not query visibility:

| interval | samples/block | block span | physical membership before sweep |
|----------|---------------|------------|----------------------------------|
| 15s      | 240           | 59m45s     | 30d 59m45s                       |
| 1m       | 240           | 3h59m      | 30d 3h59m                        |
| 5m       | 240           | 19h55m     | 30d 19h55m                       |
| 1h       | 240           | 9d23h      | 39d23h                           |
| 1d       | 30            | 29d        | 59d                              |

The daily row is the hypothetical monthly-block layout, not what the live
daily-head test produced. Even for such a block, logical retention remains
thirty days. Rewriting it to discard an expired prefix is unnecessary for exact
reads. GC frees whole blocks after their final sample expires; the bytes in a
freed SQLite page or an old backup are not a secure-erasure guarantee.

### Keep the cheap path and bound its workers

Earlier sparse packing was measured as well: seven-day safe prefixes projected
to 943.7 MiB per million daily series including the registry; fourteen-day ones
to 824.3 MiB. The head-only lifecycle projected to 769.2 MiB. Extra block rows,
payload rows and maintenance indexes cost more than the compression saved on
this workload. Leave quiet sparse data in the head and pack when its actual
count, bytes or span justify it.

Keep summaries separate from payloads. With expiry and foreign-key indexes, a
dense integer block measured 1.858 bytes per sample and a noisy float 9.132.
Inlining into a rowid table reduced those to 1.807 and 8.708 but slowed full
summary scans from 2.45/3.00 ms to 5.43/14.85 ms. An inline `without rowid`
table raised noisy-float storage to 19.565 bytes per sample. The earlier
1.55/8.94 figures omitted maintenance indexes and are component estimates.

The codec is a more useful RAM target: the default encoder/decoder pair retained
25.451 MiB of Go heap at GOMAXPROCS=16, against 1.601 MiB with one worker each
and 1.338 MiB with an 8 KiB window and lower encoder memory. Both tested payload
sizes stayed unchanged. Use shared bounded codecs; do not allocate one per
series or let CPU count choose the embedded memory budget. This is codec heap,
not the total process footprint. `measurements.md` records the full measurements.

### Retention reaches a physical plateau

A ninety-day run kept thirty days, inserting each day before deleting blocks
whose `end_ts` was behind the cutoff. The file grew once to accommodate the
thirty-first day, then reused its freelist:

| values          | day 30 allocated/file | day 31 allocated/file | day 90 allocated/file | free at day 90 |
|-----------------|-----------------------|-----------------------|-----------------------|----------------|
| whole numbers   | 32.7/32.7 MiB         | 32.9/33.8 MiB         | 33.0/33.9 MiB         | 0.9 MiB        |
| noisy `float64` | 188.8/188.8 MiB       | 189.1/195.1 MiB       | 189.2/195.3 MiB       | 6.1 MiB        |

The main file does not shrink after deletion and does not need to: physical size
plateaus about one ingest day above the live window, while freed pages are
reused. `allocated` is `(page_count - freelist_count) * page_size`; it includes
unused space inside occupied pages and is not the byte size of live samples.
This run inserted prepacked dense blocks directly. It proves reuse for that
fixture; the daily-head lifecycle above supplies the separate sparse evidence.
Every reported main-file snapshot was taken after a truncate checkpoint.

### The planner comes from arithmetic

```text
step = range / MaxPoints

30 days / 1200 px  = 36 minutes  → choose output buckets, then test containment
 2 hours / 1200 px = 6 seconds   → usually raw payload and head rows
```

An hour-long block cannot be split into exact 36-minute buckets from its
summary. Every cut, including an interior output-bucket boundary, requires raw.
The same is true when the retention cutoff cuts the first block. Thresholds
written into the code would be a bug waiting for a 600-pixel panel.
A future five-minute tier may be chosen by the same arithmetic only after its
effective resolution is part of the result.

### The series registry

Heap after building a registry and postings, with labels interned into a
dictionary and series held as pairs of ids:

| series  | plain strings | interned  |
|---------|---------------|-----------|
| 600     | 0.23 MiB      | 0.10 MiB  |
| 5 000   | 1.73 MiB      | 0.54 MiB  |
| 20 000  | 6.83 MiB      | 1.99 MiB  |
| 100 000 | 34.18 MiB     | 9.89 MiB  |

Interning is worth 3.4 times, and twenty thousand series cost two megabytes,
which is the answer to the question this whole design was afraid of. The
dictionary grows with distinct values rather than with series, so a label
carrying a request id is what breaks it — which is what the cardinality guard
is for, not a taste.

## How large this is allowed to be

The format and the query model hold about a million active series without being
redesigned. That is a design target, not a default: what an embedded
installation allows is a configured soft and hard limit, and our own telemetry
deployment configures different ones. The two are measured and promised
separately, because they are different resources:

```text
cardinality   how many series exist at all
ingest        how many samples a second arrive
```

A million rarely-updated series and a million scraped every fifteen seconds are
not the same workload — the second is 66 000 samples a second, which is a
different product. The numbers behind the first are in
[measurements.md](measurements.md); the second is measured when the block builder
exists.

What follows from the target is a rule rather than an aspiration: **nothing
scans every series**, not even as a first implementation. Postings are part of
v1, because a selector written as a scan is fine at thirty thousand series and
is a rebuild of the query path at a million.

Where the registry lives is settled by the same measurement: SQLite holds
series and postings, which is what the table above already said; every posting
list is also held in memory, at 20.4 MiB for a million and 961 ms to load;
resolving a label set to a series id is a bounded cache over the file rather
than a map of everything, which is the part that costs 150 MiB.
