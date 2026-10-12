# metrics: the engine

How the Rust engine keeps the promises of the metrics book
([api/metrics.md](api/metrics.md)): what it keeps in its file, how a sample
travels in and out, how an aggregate is answered without decoding what it
need not, and which measurements decide its format. Written on 11 October
2026, before the engine's first line, from the sources below; change it in
the commit that changes what it says.

## Sources

| What                   | Where                                                                                                                                                                                      |
|------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| the promises           | GATES.md's metrics lines, which name the Go engine's tests until Rust's land, and the book                                                                                                 |
| storage and arithmetic | the Go engine at `e81a050` (`metrics/`, `codec/`); research's [design/metrics.md][design], [design/format.md][format] and [the aggregate contract][contract]                               |
| speed                  | research's metrics-native, metrics-optimization and metrics-deep rounds of 8 October and their [synthesis][synthesis]: algorithms to take, never code to copy ([evidence.md](evidence.md)) |
| size                   | research's metrics-layout and metrics-payload rounds of 9 October, and an outside density study of the same day (harness `bb6f803`, not yet in research), which the owner holds            |

The format is the Go engine's, as measured, and changes only by a
measurement, one change at a time, on the same corpora: the owner's rule for
records. The changes below that are taken at once were measured never to
cost a byte.

[design]: https://github.com/tinyshed/research/blob/main/tinystore/design/metrics.md
[format]: https://github.com/tinyshed/research/blob/main/tinystore/design/format.md
[contract]: https://github.com/tinyshed/research/blob/main/tinystore/design/aggregate-contract.md
[synthesis]: https://github.com/tinyshed/research/blob/main/tinystore/reports/rust-research-summary-2026-10-08.md

## In one picture

```text
instruments ──(what changed, every second)──► the engine's totals ──(every 15 s)──┐
ingest ───────────────────────────────────────────────────────────────────────────┤
                                                                                  ▼
                      head: the newest samples of a series, in chunks of 240, rewritten as they come
                                                                                  │ packing, in the background
                                                                                  ▼
          blocks of at most 240 samples, cut where hours begin ──► groups of up to 32 blocks,
          values by the smallest of seven representations           a directory each: first value,
          times in a clock that series written together share       exact summary, where the body is
                                                                                  │
read, latest, aggregate: plan ──► one short snapshot of bytes ──► decode outside it, or answer
                                                                  whole blocks from their summaries
```

## The file

`metrics.db`, `durability: 'os'` unless the store opened `'full'`. The schema
is one migration, `0001`, until `v0.1.0`.

| Table          | Holds                                                                                                                           | Why this shape                                                                              |
|----------------|---------------------------------------------------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------|
| `store_state`  | the count of series, the next payload id                                                                                        | one row                                                                                     |
| `names`        | a name's id, kind, `keep`, buckets, unit and help                                                                               | what the book says a name owns, set by instruments, `ingest` and `describe`                 |
| `series`       | its id, its identity, its name, its label ids gap-coded                                                                         | the identity is the SHA-256 of the canonical labels as 32 bytes, where Go kept 44 of base64 |
| `label_values` | a pair's id, name, value and the count of its series                                                                            | the dictionary: a pair kept once, its count choosing the shortest posting list              |
| `postings`     | a pair and a series, `WITHOUT ROWID`, no foreign key                                                                            | which series hold a pair, so that nothing scans every series; a removal goes by their key   |
| `series_state` | the newest time seen, what is sealed, a version, when it is due to expire, whether retention took samples of it, ready, failure | fixed in size, with partial indexes for ready, due and failed series                        |
| `heads`        | a series' packed head                                                                                                           | its growing and shrinking stays out of the state rows; Go kept it in them                   |
| `clocks`       | a digest, the groups that use it, the timestamps                                                                                | shared by digest and bytes: series written at the same ticks share one                      |
| `groups`       | a series, its start and end, its clock, its directory; unique on series and start                                               | a rowid table, where Go's was `WITHOUT ROWID`                                               |
| `payloads`     | an id and a body                                                                                                                | bodies over 16 bytes; smaller ones live in the directory, a constant block has none         |

- The identity as 32 bytes took 12 bytes from the row and 12 from its index a
  series: −5.56% on 512 short series, never more bytes elsewhere (density
  study; Linux, Xeon 8573C, a fresh rebuild of each fixture by one SQLite
  3.53.1 writer).
- The directory no longer repeats its clock's id, which its row holds: 8
  bytes a group, a page fewer on two fixtures and none more on any (same
  study).
- A `WITHOUT ROWID` table keeps at most about 1002 bytes of a row in its page
  at 4 KiB; a directory larger than that spills into an overflow page that
  is mostly empty. As a rowid table, with the unique index paid, Alibaba's
  file fell 21.40% and TSBS's grew 0.45% (metrics-layout, 9 October; Ryzen 7
  7700 under WSL2, SQLite 3.53.4). It is taken, and measured again on the
  Rust engine's own files.
- A head out of the state rows: Go's Alibaba file kept its state rows in
  1,253,376 bytes where a fresh copy of the same rows took 188,416, the rest
  pages half emptied by tails that grew and shrank. Whether a table of its own
  keeps that space is measured on a trace of appends, packing and expiry
  before it stays.

## A sample's way in

### `ingest`

- A call is checked whole first: names and labels within their bounds, a
  kind a name already has, times inside `[now − keep, now + 10 minutes]`, no
  sample behind what its series has sealed. Its samples are sorted by time,
  and of a time given twice the later is kept.
- It is one savepoint of the file's grouped commit, with every series it
  registers: identity, dictionary pairs and postings in the same savepoint.
  A refused series names itself and the call writes nothing.
- A head is chunks of at most 240 samples behind one header and a checksum
  bound to the series, one parser for the layout. A write decodes and
  re-encodes only the chunks its samples touch; the others keep their bytes.
  A head holds at most 16,384 samples and 1 MiB, so that the backlog of one
  call fits; past it is `limit` until packing catches up.
- The newest time a series has seen moves once, at the end of a call, so
  that a backlog does not refuse its own samples halfway through. A series is
  marked ready once the first unit of the ladder its head starts in lies
  behind that time: a guess from the head's extent, which packing checks
  against the samples.
- A series is found by its identity alone, the SHA-256 of its labels each
  behind its length: where the Go engine confirmed a match by the label ids
  too, a collision of SHA-256 is not a case this engine handles.

### Instruments

- In Rust an instrument is a handle on the engine's totals. A set of labels
  is a slot found through a hash of its canonical labels; a counter's slot
  adds with an atomic operation, so `inc()` takes no lock, and a write takes
  what calls added with an atomic swap. A timer's slot keeps its count, sum,
  longest and buckets under a lock of its own, so that a write takes them
  together.
- An SDK keeps what changed since it last sent, and sends it every second,
  or sooner once its buffer fills, as one message. A connection declares a
  series once and is given an id; what it sends afterwards is ids and
  numbers.
- The engine's totals hold, for each series, a counter's total, a timer's
  count, sum, longest and bucket counts, a gauge's value and the connection
  that set it, and its run.
- Every 15 seconds, at `flush` and when the store closes, the store writes
  every live total at one time for all of them, so that their clocks are
  one: 256 slots a transaction, each slot's series in a savepoint, so that a
  timer's go together and what the store refuses of one slot is said once
  while the others are written. A write that fails whole is tried at the
  next, which writes the totals that grew meanwhile.
- A run begins at the first call after a write that found none, and its
  zero is written at that write, the last time the series is known to have
  been zero, or at the engine's start: a millisecond before its first count
  at the latest. The zero lies after every sample its series holds, and the
  count after the zero, so that a store opened again within the millisecond
  it closed, or a clock moved back, never writes over the run before.
- A gauge whose connection left is dropped from the totals; a counter's, a
  timer's or a histogram's series that nothing sent for an hour leaves them
  after one more write, its slot freed unless a handle holds it, and its next
  send starts a new run. An instrument takes 2000 sets of labels between two
  sends, the set without labels none of them, and past them counts into the
  series of `__overflow`.
- A gauge's function runs where it was written: in an SDK on its own timer,
  never twice at once, a slow call skipped that time and an error said once;
  in Rust on the thread that writes, which it must not block, an error, a
  panic or a value that is not a finite number writing nothing that time.
- The recorder of the `metrics` crate opens an instrument a name the first
  time a macro names it, with what `describe_*!` said and the buckets it was
  given; a name of another kind counts nowhere, said once.
- Closing the store writes the totals last. A crash of a program loses what
  it had not sent, a second at most; a crash of the store, what it had not
  written, 15 seconds at most, and the next run's zero keeps `increase`
  right.

### Exponential buckets

An experiment before the native histogram, which keeps a set's buckets in
one sample and comes after `v0.1.0`: a timer given `Buckets::Exponential`,
`buckets: 'exponential'`, lets the store keep its spread.

- The grid is Prometheus' native histograms': the bounds 2^(i/2^schema) for
  every integer i, an interval (bound(i − 1), bound(i)] for each, and 0 an
  interval of its own. The bounds come from Prometheus' table for schema 3,
  never from `pow`: three of them lie a unit in the last place below the
  float nearest 2^(j/8), and a value on a bound falls where Prometheus puts
  it, so that a native histogram scraped later merges with these exactly.
- A coarser schema's bounds are every other of the finer's, the same floats,
  so interval i of the finer lies whole in interval ⌈i/2⌉ of the coarser and
  two resolutions merge exactly, with nothing moved across a bound.
- A timer counts each value into the interval of schema 3 it falls in,
  eight a doubling, about 9% wide. Each interval a value fell in is a series,
  `_bucket{__bin="3:13"}`, its schema and its index in the store's own label,
  counting its own values and not every value at most a bound: a cumulative
  count changes at every bound above a value, so only counts of intervals
  leave the empty ones without a series, and `le` keeps its meaning.
- An interval is a run of its own: it begins with its zero at the write
  before the one that first finds a value in it, is written at each write
  while it lives, and ends once none fell in it for an hour.
- A set's intervals past 128 lower the schema: the finer end where they
  are, and the coarser begin at zero with that write's values, as a run
  begins, so that no value is counted twice; at schema 0, one interval a
  doubling, the spread of a float bounds them.

## Packing

- Background work through `Host::every`, each minute and when the ready
  index holds work, a bounded number of series a pass and a group a series;
  another pass follows at once while one left series ready or a series more
  complete units than a group takes. `background: false` stops it, and a
  test packs through the engine's maintenance.
- What may be packed is a prefix: the samples before the series' newest
  time less the engine's lateness, zero as the Go engine's until a
  measurement of late arrivals says otherwise. A quiet series is never forced
  into a block; its head may expire whole.
- Blocks are cut where a unit of this ladder begins, in UTC: 1, 5, 10, 15
  and 30 minutes, 1, 2, 3, 4, 6, 8 and 12 hours, a day; and below a minute,
  for a series faster than four samples a second, 200 and 500 milliseconds,
  1, 2, 5, 10, 15, 20 and 30 seconds, so that no head waits for a unit to end
  with more samples than it holds. Each divides a day.
  A series takes the largest unit that holds at most 240 of its samples at
  the mean step of what it packs, a unit that holds more cut every 240:
  every 15 seconds is a block an hour, 240 samples; every minute, 4 hours;
  every 10 seconds, 30 minutes of 180. A block never crosses its unit, so a
  bucket of whole units holds whole blocks and answers from their summaries.
  The Go engine cut 240 samples wherever they began, and an hourly bucket cut
  nearly every block; what this costs in bytes is measured against Go's on
  the corpora below before it is kept.
- A candidate is read in a snapshot and encoded outside the writer: each
  block's value representation, its clock, its directory entry and exact
  summary. Publication then takes the writer for one transaction, and checks
  the series' version: the payloads, the directory, merged into the group
  before it while that has no more live blocks than it, fits 32 blocks, a
  clock of 64 KiB and a directory of 8 KiB, so that a group at least doubles
  each time it is rewritten, the clock's references, the head trimmed by
  exactly the samples packed, and what is sealed moved past the last of
  them. A transaction carries at most 8 series and 1 MiB, each series
  in a savepoint of its own, as the Go engine's did.
- A series whose bytes no longer read is suspended, with what broke, said
  once; the others go on. `damaged()` lists it and `delete` removes it.

## The codec

One byte layout, one parser, each: a head chunk, a clock, a value body, a
directory, an exact summary. Decoding, reuse and partial reads start from
what that parser returns. What records packs with too, varints and their
bounded reader, bits read a word at a time, zstd and CRC-32, is the root's
`compress`, which both engines stand on.

- **Times** live in clocks: none for evenly spaced samples, else the
  distances between them or their changes. A clock is shared by digest, and
  its bytes are compared before it is.
- **Values** take the smallest of: a constant, no body; the changes alone; an
  integer path, ZigZag deltas in Simple8b words; a decimal grid, scale 0 to
  15, whose integers take that path and whose values off the grid keep a
  residual of their bits, the idea ALP has with exceptions; Gorilla's XOR;
  raw bits. Each may go through zstd, its window 8 KiB. A series keeps a hint
  of its grid's scale for its next block.
- A body's checksum binds it to its block's head and clock, so that one
  block's values never read as another's.
- **The directory** of a group: its slots, the masks of slots alive and
  slots given rows, and for each block its first value, flags, the summary
  fields its prediction misses, resets, the exact sum and increase, and its
  body inline or its payload id. Its checksum binds the series, the start and
  the end. A payload id never moves, so that a merge rewrites directories and
  no body.
- **Exact sums** are integers in units of 2⁻¹⁰⁷⁴, kept canonical in a
  directory, and folded in a fixed accumulator of 35 64-bit words, rounded to
  a float once: research's cut-block sum went from 2.02 to 0.82 ms with it,
  where every sample had made a big integer.
- **Bits** are read a word at a time: research's 239 residuals of 13 bits
  went from 4.01 to 0.32 µs.
- **huff0**, which the Go codec weighed against Simple8b for integer deltas,
  is first turned off on the corpora: if it saves crumbs, as FSE did in
  records, it goes; if not, it is a safe Rust huff0 of the engine's own.

## Reading

- **A plan** comes first, from postings, the shortest list first, then each
  series' directories and head the range touches: which blocks answer from
  their summaries, which decode, the bytes it reads and the samples it
  decodes, and the first bound it passes. `explain` returns the plan, and
  `read` and `aggregate` carry it out, so that what a plan says is what the
  call spends. A plan leaves every payload in the file, counted by its
  directory. Past the samples a call may decode it goes on counting, which
  costs it nothing, so that `exceeds` says how many the call wants; at the
  series it finds and the bytes it reads, which are its own work, it stops
  where the call would.
- **A snapshot** then copies bytes only, directories, clocks, payloads and
  head chunks, inside one read transaction, each row's bytes checked against
  what the read may still copy before it is, and ends before anything is
  decoded. The query's weight, the bytes it copied and the samples they
  hold, is then reserved from the store's memory before it decodes, waiting
  for it as records' reads do.
- **Decoding** runs on the caller's thread: an evenly spaced clock fills its
  times without reading, values decode straight into columns. Research's
  parallel decode helped some reads and slowed others; it waits for a
  measurement on this engine.
- `read` gives columns a series; `latest` decodes one head chunk, or answers
  from the last block's directory, a series: the newest chunk the range
  touches, else the newest block, decoded only when the range ends inside
  it. A series whose newest group ended before the range costs no directory.

## Aggregates

- Buckets of `every` counted from 1970 in UTC, clipped by the range and by
  each series' `keep`. `at` is where a bucket begins on that grid, for one
  the range cuts too; without `every` it is the range's start, and a query
  that says no start begins where the name's `keep` does.
- Each series has an accumulator a bucket for its `op`: a count; an exact
  sum; the least and the greatest, `-0` before `+0`; the first and the last,
  bit for bit; an increase, a rate and a delta, which carry the previous
  sample into the next bucket, the step rule; and its resets, `partial` and
  `gap`. A histogram's `_bucket` series is a counter to every `op`.
- `gap` is a first step that began before the bucket before its own; without
  `every` the yardstick is the range's own width.
- `partial` is a bucket whose value comes from less than its span: one the
  range cut, one whose end the store's clock has not reached, one that begins
  before the oldest time its name keeps; and, for an `op` that counts steps,
  the bucket of a series' oldest sample when retention took samples of the
  series, since the step into it began at one. `series_state.trimmed` says
  it took some, set in the transaction that expires a sample, and so does a
  sample the file still holds though its name keeps it no longer. A series
  that begins in the range lost none. A rate still divides by what the range
  leaves of a bucket: the flag, not the divisor, says it is no whole one's.
- Of the samples before the range only the newest is taken, and checked: an
  older NaN there refuses nothing.
- A block wholly inside one bucket and the clipped range answers from its
  summary, and its body is never fetched; one cut by either decodes. The
  first bucket's step comes from the newest sample before the range, found as
  `latest` finds it, a whole block's from its directory.
- A group joins its series' buckets exactly and rounds once; the series of
  `__overflow` is a group of its own. Groups are answered one at a time, so
  that the store's memory holds one group's exact sums, a fixed 35 words a
  bucket, reserved as its buckets come.

### A timer by its name

- A name without series of its own whose `_count` has some is a timer's: a
  query of it reads, in the same snapshot, the series the timer writes that
  the call needs. `read` and `latest` give all of them, each by its own name.
- `count` and `rate` are its count's increase, `sum` its sum's, `avg` the one
  over the other, exact, `max` the greatest of its longest, and a percentile
  its buckets'; `samples` is how many durations, and `resets` and `gap` its
  count's. A bucket in which it measured nothing counts 0 for `count`, `sum`
  and `rate`, and is left out of `avg`, `max` and a percentile, which have
  nothing to take. Any other `op` is `invalid`, and so is one whose series the
  timer keeps none of: a percentile without buckets, the longest of a
  histogram a scrape brought.

### Percentiles

- A rank is the decimal the query wrote, `p99.9` as 999 in 1000, at most 15
  decimals, so that no float's rounding decides which sample or bucket holds
  it: 0.07 × 100 is 7.000000000000001 as floats, whose ceiling takes the
  eighth sample where the seventh holds the rank.
- A gauge's percentile gathers its bucket's samples, every series' of the
  group, within the decode bound, and selects the rank without sorting them
  all, ⌈rank × n⌉, `-0` before `+0`: no block answers from its summary, which
  holds no rank.
- A timer's percentile counts each `_bucket` series as a counter, a block
  inside one bucket from its summary, so that hours over a month decode
  almost nothing.
- A member of the group is one set of the timer's labels. A writer writes
  every bound of its set at each write, so in a bucket of time a member holds
  the bounds whose series counted as many samples there as its `+Inf`: a
  bound of a set written for part of that time counts fewer. The group keeps
  the bounds every member holds, their counts added exactly, and the rank is
  the first whose count reaches it: `upper`, and the one before it `lower`,
  0 below the first. A member that counted no value there takes no part, so
  that a process idle in the bucket narrows no other's bounds; one with
  buckets and no `+Inf` there is `invalid`, its values not all counted.

  ```text
  one member's hour   +Inf 5 samples   [10, 20, 50, 100] 3 each, then [5, 25, 75, 150] 2 each
  the bounds it holds there: +Inf alone, so the rank lies above 0 and at most Infinity
  ```

- The intervals of exponential grids join at the coarsest schema among
  those that counted a value; beside bounds a program gave, the rank is found
  among the bounds both have, and every grid has 0 and the powers of two.
- `estimate` interpolates within the bucket that holds the rank as
  Prometheus' `histogram_quantile` does, in floats: past the last bound it is
  that bound, and with no bound below, none.

## Retention

- Each series is due at its oldest sample plus its name's `keep`, one
  indexed column; a pass takes a bounded batch of what is due.
- A head's expired samples go at once; a block goes once its last sample
  has expired, and a group once it holds none. A group partly expired keeps
  every payload's address, its live mask telling which still count.
- A series left with nothing goes with its postings and the dictionary pairs
  no other series uses, and frees its place among the 100,000. Its postings
  go by their key, from the pairs its row names: they have no index by
  series, nor a foreign key that would walk them all to remove one.
- A name whose `keep` changed between two runs moves the due time of its
  series through the name, never by scanning every series.

## Concurrency and memory

- One writer for `metrics.db`: `ingest` calls and the instruments' writes
  share grouped commits, and packing publishes through the same writer in
  short transactions.
- Readers are `query_only`, and a snapshot lasts only as long as copying its
  bytes.
- What the engine holds in memory has a bound: a query its reservation,
  instruments 2000 sets each between sends and 100,000 series in all, idle
  series evicted after an hour.

## The wire

- `protocol/metrics.wire`: `read`, `latest`, `aggregate`, `explain`, `names`,
  `labelNames`, `labelValues`, `description` and `damaged` are read methods,
  which a `read` token may call; `ingest`, `describe` and `delete` write; a
  connection declares its instruments' series and sends what changed.
- A read answers in parts within the client's credit, as columns, as
  `sql.columns` does.

## What measurements decide

1. **The port against Go `e81a050`**, on the same inputs: TSBS, Alibaba,
   the density study's six fixtures and a production metrics dump; the file
   divided by object, `dbstat`'s pages, and the workloads of metrics-native.
2. **One change at a time**, each against the step before it: groups as a
   rowid table, heads apart, blocks cut at the ladder's units, huff0 off.
3. **A codec round**: which representation each block took and its bytes; each
   representation turned off in turn; then candidates one by one: NaN and
   infinities as residuals of the grid rather than a reason to leave it,
   Chimp's or Elf's XOR, bytes shuffled before zstd, words of one width read
   with SIMD; pco and ALP on the same series as references only.

Not taken, by what was measured: 8 KiB pages, which won only on hard floats
and cost every other fixture up to twice its bytes; payloads packed together,
which held dead bodies and slowed narrow reads; bodies inline up to 32 bytes,
a gain on Alibaba alone; a planner choosing layouts per series, which the
density study found unjustified.

## Order of work

| Slice | Builds                                                                                                         |
|-------|----------------------------------------------------------------------------------------------------------------|
| 1     | the schema, the registry and postings, `names`, `ingest` into heads, `read` of heads, `keep` and expiry        |
| 2     | the codec, clocks, directories, exact summaries, payloads, packing and publication; `read` across blocks       |
| 3     | `aggregate`: buckets, every `op`, the step rule, `gap`, `partial`, groups; `latest`; `explain`                 |
| 4     | instruments: the engine's totals, runs and their zeros, overflow, eviction, gauges by connection; the recorder |
| 5     | percentiles of gauges and timers; buckets and their shared bounds; a timer by its name; exponential buckets    |
| 6     | `names`, `labelNames`, `labelValues`, `describe`, `delete`, `damaged`                                          |
| 7     | `protocol/metrics.wire` and the Bun SDK                                                                        |
| 8     | fuzz targets of every parser, GATES.md's lines in Rust, the measurements above                                 |

After: Prometheus' text (C5), rollups (C4) and `poll` (C6), as the book has
them.
