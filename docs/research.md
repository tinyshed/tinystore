# What is not built

Nothing in this file works. Each entry is a hypothesis, an arithmetic estimate
or an open question, and each one says what it would have to show to be worth
writing. Where a number here is arithmetic over two measured numbers rather
than a measurement, it says so; the measured ones are in
[measurements.md](measurements.md).

## The physical shape of the head

Update: the first public-engine packed head is now implemented and measured in
[packed-head-2026-09-21.md](reports/packed-head-2026-09-21.md). The rationale and sparse
acceptance targets below predate it; the million-series sparse experiment and
full WAL/RSS comparison are still outstanding.

Measured today: a durable head of one row per sample, and immutable blocks
behind it. The candidate is to put the recent samples inside the series row as
one packed, rewritable byte string, and to make everything behind it an
immutable segment:

```text
        measured                          candidate

  series_state row                   series_state row
  head row                             ├── max_seen_ts
  head row                             ├── sealed_before
  head row                             └── packed tail  [● ● ● ●]
  head row                                     │
       │                                       │ safe prefix
       ▼                                       ▼
  immutable block                        immutable segment
```

What it would buy: one table instead of two, one row rewritten per batch
instead of a row per sample, and — on the sparse fixture — most of the write
amplification. What it costs: the tail is rewritten whole on every batch, and
SQLite writes whole pages into the log, so a small append is not a small write.
That is exactly what the experiment has to settle.

**A fixed tail size cannot work, and this is the trap to avoid.** The tail must
hold the whole window that may still change *plus* enough safe samples to make
a segment worth sealing:

```text
HardTailSamples  >=  lateness window  +  target segment

15s cadence, 5 min lateness:     21  +  240  =  261
1 Hz,        5 min lateness:    301  +  240  =  541
```

A 32-sample tail with a 5-minute window leaves room for about eleven safe
samples, so it seals eleven at a time and quietly recreates the scatter of tiny
objects it was meant to remove. And no finite tail survives an unbounded rate:
at 100 samples a second the unsealable window alone is 30 000 points. So the
budget is three separate knobs, in bytes as well as samples, because 261
constant values and 261 random floats are not the same object:

```text
TargetSegmentSamples     how large a sealed segment should be
TailFlushThresholdBytes  when it is worth trying to seal
HardTailBytes            when the store must refuse or overflow
```

The first version refuses with a resource error when the hard budget is reached
and no safe prefix exists. Sealing unsafe samples to stay inside a limit is not
an option, and a quiet overflow into rows is the old head coming back through
the window.

Sealing stays one transaction with a compare-and-set, because encoding a
candidate outside the writer is the only way not to hold the single writer
while zstd runs:

```text
read tail and version
encode candidate outside the transaction
BEGIN IMMEDIATE
  version unchanged?  no  → retry, bounded
  yes → insert segment, replace tail, move sealed_before, bump version
COMMIT
```

Every mutation of the candidate bumps the version — ingest, a replaced sample,
retention. A version that only tracks ingest gives false confidence. The retry
is bounded, or a hot series turns the compactor into a benchmark of two
goroutines arguing.

**Acceptance.** Measured baselines for one million daily series over thirty
days, including the earlier registry: a row head is 771.8 MiB and 641.4 WAL
bytes per input sample; immutable chunks of 8 are 620.2 MiB and 268.5. The
candidate is worth the complexity at:

|                            | accept    | target  | stretch |
|----------------------------|-----------|---------|---------|
| total on disk              | ≤ 400 MiB | 350 MiB | 300 MiB |
| WAL bytes per input sample | ≤ 300     | 250     |         |

measured with identical batching and checkpoint policy on both sides, and with
a dense series and a filled lateness window in the same run as the daily one.
The daily case alone would only prove it is a good format for telemetry.

## Where the rest of a dense block's bytes are — answered

Update: built past what this section asked for. Blocks have no rows of their
own any more — a group row holds up to 32 — so neither `block_payload` nor
`block_expiry` exists, and retention walks series through
`series_state.next_gc_ts` and its partial index `series_due`. The numbers below
predate it, and a retention pass at a million series is still unmeasured.

Measured rather than argued about, and the section that used to stand here
guessed wrong twice. [measurements.md](measurements.md) has the division of the
file; what it leaves for this document is what to do about it.

`≤1.0 bytes a sample` on dense whole numbers is reached without touching the
codec: **0.964, by removing two maintenance indexes.** Neither removal is free,
and each one is a design change with a gate attached rather than a tuning
knob.

`block_payload` is an index over a foreign key, 19.7 bytes a block, whose only
job is to let a deleted payload find the block pointing at it. Deleting both in
one transaction does the same job for nothing — at the price of the database no
longer enforcing it, so it needs a test that a deleted block leaves no payload,
and a second one that the reverse cannot happen either.

`block_expiry` is 24.2 bytes a block for finding expired blocks across the
whole file, and this one is **not** yet safe to remove. The size measurement
answers "does SQL still work without it", which was never the question. The
question is what finds due work instead:

```text
series_state.next_gc_ts, partial index where it is not null
        ↓  which series are due
primary key (series_id, start_ts)
        ↓  which of its blocks are old
delete
```

That is one index entry per series instead of one per block — a large win where
a series has many blocks, a wash where it has one. Its costs are a row update
per sealed block and a retention pass that walks series rather than blocks, and
until both are measured at a million series with their throughput and their
p99, `0.964` is a number with a debt against it. What is safe today is
`1.046`: that one only drops the foreign-key index.

The measurement has one more obligation, and it is the one easiest to skip:
**it reports the size of the index it introduces.** Deleting 24.2 bytes a block
and creating an unmeasured number of bytes a series is moving bytes into the
next pocket and calling it a saving. `dbstat` divides the file by object, so
there is no excuse for reporting a total.

What this does **not** license is deleting the summary. It is 24.6 bytes a
block on whole numbers and 61.4 on noisy floats — the largest single item after
the payload — and it exists so that a whole-block query never decodes. Its cost
is the answer to a different question: whether a coarse tier is worth it.

There is a smaller question underneath it. A gauge is answered by `min`, `max`,
`sum`, `first` and `last`; a counter by `first`, `last`, `increase` and
`resets`. Carrying all seven for both is about two columns of waste, which on a
noisy float is 16 bytes a block. Whether that is worth a second row shape
depends on facts we do not have — how many series of each kind a real
installation holds, and how often a whole-block summary is what a query
actually reads. That belongs after the corpus, not before it.

Two numbers follow from the same measurement. On tenths of a degree the file is
2.342 today, so the same two index removals put it at 2.159; with the scaled
integers below at their estimated 0.42 payload it would be about **0.787 bytes
a sample** — the stretch target, by arithmetic, on the shape real gauges have.
On noisy floats the same removals give 8.936, and nothing short of losing bits
moves that much further.

The page sweep the reports asked for is still worth running, but it answers a
different question: not what a row is made of, which `dbstat` now answers
exactly, but where the packing cliffs are between one payload size and the next.

## Exact decimals — built

`codec/` has a fifth value representation: a decimal scale and then the integer
path. [format.md](format.md) says what it writes, [measurements.md](measurements.md)
what it cost. The short version is that the arithmetic in this section was
close and slightly pessimistic — it predicted 0.42 bytes a sample on tenths of
a degree and the measurement is 0.379, and the whole file for that fixture went
from 2.342 to 0.865 with the schema untouched.

Three things the build settled that the estimate had not.

**The ceiling is not decoration, and it is not a law either.** With no limit on
the scale, a noisy float walk also has a decimal form that divides back exactly
— thirteen digits of it — so the encoder spent 95.9 microseconds building a
representation it then threw away for being too large. Nine places brought that
back to 15.3 and changed no payload size anywhere, which is all the evidence
there is: it says an unbounded search is expensive, not that nothing useful
lives past the ninth place. An exporter may well emit ten or twelve exact
decimal places. So the analyzer below owes a distribution before this number is
defended:

```text
the best exact scale of each block in the corpus
    k = 0, 1, 2 … 9, more than 9, none at all
```

If more than nine is a rounding error, the ceiling stays. If it is four
percent of a real corpus, it moves, or the search gets cleverer than counting
upward.

**Rounding the product, not truncating it, is what makes it work at all.** Of
the 9999 two-place decimals, 573 have a product that lands just under the
integer they were written as — `0.29 × 100` is `28.999999999999996`. Truncating
refuses each of those, and a block is all or nothing, so a 240-sample block of
two-place decimals would survive truncation with probability about seven in ten
million. The bitwise round trip is what keeps rounding honest: it decides, not
the multiplication.

**The selector has to stay in charge.** A fixture of what looked like
hundredths turned out to be whole numbers in disguise, and raw values under
zstd beat the scaled path on it. That is the right answer, and it is the reason
the encoder compares finished candidates rather than classifying the data.

**Golden vectors now exist**, one per value representation, and they are the
gate that a released format cannot move: fixed bytes, the samples they must
read back as, on every platform CI runs.

What is left here is not the codec. Decimal gauges are at 0.865 in the file
today and 0.783 once the foreign-key index goes, and the remaining two thirds
of that is the block row and the page it sits on.

## Predictors and residuals

Two pieces of arithmetic to do before writing four predictors.

**Differencing amplifies noise.** For independent noise of variance σ²,
`Var(Δ) = 2σ²` and `Var(Δ²) = 6σ²`, so each order costs 0.5 and 1.29 bits a
sample before it has removed anything. A second-order predictor has to remove
0.79 bits more trend than the first just to break even, which is why the
selector must choose on the size of the packed result and never on a heuristic
about what kind of metric this is.

**Simple8b pays by the widest value in a word**, so what to measure is the
distribution of per-word maxima, not the mean residual. But the reports
overstate the case for a patched scheme: our packer already takes the widest
selector that fits, so a single outlier costs one 8-byte word and the values
after it repack densely.

**XOR against a predicted reference wins less than it looks.** On a smooth
signal sampled 240 times a period, consecutive values differ by about 2^47
units in the last place and second differences by about 2^43, so the meaningful
XOR window is roughly 48 bits and the per-sample control bits take it to about
61 — which is why the measured cost is 63.6 bits, or 7.946 bytes a sample.
Moving the reference to a linear prediction removes some leading bits and keeps
every control bit, and cannot take a second difference at all.

The alternative is cheaper and reuses what exists: map the float's bits to an
order-preserving integer — flip every bit when the sign is set — and send the
second difference through the integer path. By the arithmetic above that is
about 44 bits a sample, or ~5.5 bytes against 7.946, roughly a third off.

It has one blocker in our own format: Simple8b's width ladder runs
`… 15, 20, 30, 60`, so a 44-bit residual rounds up to 60 and loses to raw. The
idea needs a bit-packer of arbitrary width, which `bitWriter` already is.

**Nothing of this gets written before an analyzer says there is meat in it.**
The smooth signal above is our own generator, and real monitoring is mostly
decimals and whole numbers, where scaled integers win and this does nothing.
The instrument comes first, and it is small: run over real blocks and report,
per block and in aggregate,

```text
bits a sample under XOR, as we encode it now
bits a sample under an ordered-integer first difference
bits a sample under an ordered-integer second difference
the same three packed ideally, against the same three packed as we pack them
```

The last pair is the one that decides whether the work is a codec or a packer.
If the ideal is 3.8 bytes a sample against the 6.2 we have, an arbitrary-width
packer is worth writing. If it is 5.9 against 6.2, the idea dies without
costing a line of storage code.

## One time axis for many series

The reports call a shared cadence with a presence bitmap the structural
advantage over per-series engines. On the data we have it is not:

```text
fixed step, per block   8 B first timestamp + a varint step ≈ 13 B / 240
                        = 0.43 bits a sample
presence bitmap         1 bit a slot
```

More than twice as expensive as what we already do. And on the jittered fixture
timestamps cost 1.92 bytes a sample — 15.3 bits — but that is real jitter
rather than missing slots on a grid, and a bitmap cannot represent it.

What a shared object could amortise is not the axis but the 103 bytes above:
one row for fifty series instead of fifty rows. That makes it a layout
candidate for the sparse case — a competitor to the packed tail, measured in
the same table — and not a codec. Its cost is the one the reports admit: one
late sample rewrites an object belonging to fifty series.

## Entropy coding — measured, and larger than expected

Update: built. The codec writes a block's deltas as Simple8b words and as a
`huff0` stream with its own table, and keeps the smaller;
[format.md](format.md) has the byte. What follows is the measurement that
justified it.

The version of this section that guessed put the prize at 0.056 bytes a sample
and filed it under P3. The measurement in [measurements.md](measurements.md)
says otherwise: between what we write and the order-0 entropy of the delta
alphabet there is 20 to 35 percent, and `huff0` reaches within 6 to 9 percent
of that bound with its table included.

```text
counter deltas    5.36 bits today   3.77 with huff0   3.46 is the bound
integer deltas    4.26              3.36              3.17
```

In bytes a sample that is 0.199 off a counter's payload and 0.113 off an
integer walk's — a fifth to a quarter of the whole payload, on the data class
this store exists for. It is also the cheapest thing on this page to reach for:
`huff0` is a package of `klauspost/compress`, already in the module because of
zstd, so it costs no dependency and no new graph.

What has to be established before it goes in:

```text
decode cost per sample, against the 2.6 microseconds a block costs now
the table's cost on a short block — 8 and 16 samples, not only 240
a corpus, because our alphabets are synthetic and real ones are more skewed
behaviour when the alphabet is not small, where it must decline rather than bloat
```

And it is a stream sub-encoding rather than a new value representation: the
integer and the scaled paths both produce ZigZag deltas, and either can be
written with Simple8b, with a fixed width, or with `huff0`, whichever comes out
smallest.

## A fixed width beside Simple8b

Worth having, worth nothing on its own. Simple8b charges the widest value in
each word, a fixed width charges the widest in the block, and the measurement
says that is 0.26 to 0.36 bits a delta on three fixtures and a loss on the
fourth. So it is a second candidate, not a replacement, and its real value is
that it removes the ladder's gaps — `… 15, 20, 30, 60` — which is what would
otherwise send a 44-bit ordered-integer residual to 60 bits.

## Stop paying twice for what the block row already holds

Update: built. A block's value body carries no envelope: its first value and
body length are in the group directory and its timestamps in a shared clock,
and since `cec241b` the codec says so itself, in `EncodeValues` and
`DecodeValues`.

The payload begins with eight bytes of first timestamp and eight bytes of first
value. The row it will live in has `start_ts` as its primary key and `first` as
a summary column. The payload also carries its own sample count, which the row
has as `count`, and a fixed step, which is `(end_ts - start_ts) / (count - 1)`.

```text
first timestamp   8 bytes
first value       8
count             2
step             ~3
                ────
                ~21 bytes a block, or 0.0875 a sample
```

That is 13 percent of a dense integer payload and 23 percent of a decimal one,
for no new algorithm at all. The envelope has a little more in it: three mode
bytes fit in one, the `TS` magic duplicates what the checksum already proves,
and of the two stream lengths the second is the body minus the first — call it
seven bytes more.

The cost is that the codec stops being self-contained: it has to be told that
the caller is keeping the first sample. That is not a boundary violation if it
is stated properly — a block is metadata and a body, and the body need not
repeat the metadata — but it is an API decision and it belongs to whoever is
building the store, not to the codec.

## Corpora, and what beating another engine would have to mean

A comparison counts only when both sides see the same samples, keep them for
the same time, and are measured whole:

```text
same corpus, same samples, same retention
exact float on both sides — VictoriaMetrics with precisionBits = 64
the entire footprint: payload, index, registry, and a stated WAL policy
both numbers produced in the same run, on the same machine
```

Our payload against somebody's blog post is not a comparison. TSBS first, for a
deterministic generator and a devops workload; an OpenTelemetry demo capture
second, for telemetry families nobody tuned for us. A corpus never enters the
repository.

## What is still unknown

Every measurement so far is synthetic. The numbers that need a living v0:

- ~~how the write-ahead log behaves while blocks are being packed~~ — measured
  twice. The invariants held over 71 202 snapshots at 30 000 samples a second,
  and the log is governed by the oldest open read transaction rather than by
  how often anyone reads: three readers behaving like panels left it at 4 to 9
  MiB, and one reader holding a snapshot for twenty seconds took it to 221 MiB,
  of which under 2 MiB was live. So a read transaction needs a deadline, the log
  needs a size limit, and the file is given back by a `truncate` checkpoint
  taken when nobody is reading. [measurements.md](measurements.md) has the numbers;
- `on delete set null` on an indexed key, which must not turn every retention
  pass into a table scan;
- whether the series registry holds its shape when labels are not as repetitive
  as a benchmark's;
- ~~what a sample really costs~~ — measured, and it is a question about the
  data rather than the format: 0.18 bytes for a constant, 1.25 for whole
  numbers, 7.4 for a `float64` random walk and 8.1 for a counter of them. The
  axis is how many significant digits the values carry, not gauge against
  counter; integer-like operational metrics measured 1.25 bytes, while real
  metrics also include both stable and noisy floating-point values;
- ~~what a range means after raw retention~~ — the first version does not have
  a coarse tier: raw lives as long as its block, whole blocks use their summary
  and blocks cut by a range use their payload. A future rollup is explicitly a
  coarser result, not a transparent replacement for raw;
- ~~how a sparse tail closes and what retention promises~~ — a quiet tail can
  expire directly from the durable head, with no idle sealing timer. Every read
  clips head and blocks at its cutoff; GC may reclaim an overlapping block
  later. Ninety-day daily-head runs at 20 000 and 100 000 series plateaued and
  project to about 769 MiB with the previously measured million-series registry;
- ~~what closes a block by size~~ — measured: a block is decompressed whole, so
  below a panel's window the per-sample cost doubles and above it every extra
  sample is decoded for nothing, eight times over at 1920. The two meet at the
  window, so `targetSamples` is 240 and `maxBytes` is 2 KiB, at 6.4 µs and 6.7
  KiB an hour of one series;
- what the defaults are for how many series an installation allows. A million
  is what the format holds, and what a deployment permits is named once the
  block builders and the query working set are part of the measurement rather
  than the registry alone;
- how a subject is physically encoded, which is an events question and does not
  hold metrics up.

The first slice left out five-minute buckets, the automatic choice between
resolutions, coarse retention tiers, `rate`, `increase`, the scheduler and
materialisation. `AggregateIncrease` now computes an exact answer by decoding
raw; its versioned summary shortcut remains open. Raw payload still stays until
the block is deleted. The foundation these features sit on is one ingest
path that takes any number of samples at any timestamps, the block builder, the
codec in both directions, the series registry with its postings and its
cardinality guard, and whether a series is a gauge or a counter. A first version
without those is not a first slice of this engine — it is a second engine,
shaped by whatever arrived first, with production data already on top of it by
the time the real one is written.

A counter is why the block summary is not only `count/min/max/sum`. A monotone
transition adds `current − previous`; a reset adds `current`, including the
transition between blocks. First and last values cannot recover the internal
reset deltas once raw expires. Keeping raw
does not make the summary optional: a future whole-block shortcut should avoid
the decode without changing the raw-decoding answer, and a coarse tier must not
change it either.

Events and metrics take a file each:

```text
data/
├── app.db         sources, queries, dashboards, accounts
├── events.db      event types, dictionaries, the per-type tables
└── metrics.db     series, postings, the head, blocks and payloads
```

They differ in how their schemas move, in what retention means to them, in what
a corruption costs and in how they are written, and the one operation that
would have wanted a single transaction — materialising a metric out of events —
survives being two, because recomputing a window is idempotent. Marrying two
engines for ever to save one commit is the wrong trade, and
`data/metrics/v1/metrics.db` holding events was a leftover of when there was
one file rather than a decision.

## Traps in the attached research

Two things in the reports cannot be copied as they are written.

Their example compaction transaction omits `sealed_before`, so a late sample
can land inside a range that was already packed. And if encoding moves outside
the writer, every mutation of the candidate — an ingest, a replaced sample, an
expiry — has to invalidate its version; a check that only tracks ingest gives
false confidence. The watermark stays an eligibility calculation and never
replaces the persisted frontier as the rule that refuses a sample.

A streaming iterator does not bound a query either: reading every payload
before releasing the snapshot exhausts memory just as well. And SQLite `real`
columns do not establish that a head preserves NaN payloads or signed zero — a
head that stores values as numbers must either store bits or refuse those
values at its boundary and say so. The codec itself preserves all of them.

The published work behind the parts we did take: SQLite's
[file format](https://sqlite.org/fileformat2.html) for the different overflow
thresholds of table and index B-trees, its
[foreign keys](https://sqlite.org/foreignkeys.html) for why a child key needs
an indexed lookup, and [Gorilla](https://www.vldb.org/pvldb/vol8/p1816-teller.pdf)
for the XOR transform. None of them is a dependency.

## Order of work

```text
done decompose the 103 bytes        two indexes, and 1.046 is safe today
done exact decimals                 1.792 to 0.379, and the file to 0.865
done retention that walks series    series_due; unmeasured at a million series
done the packed tail                the sparse run at a million series is still open
done a real corpus and an analyzer  four corpora, and the census of 22 September
done huff0 over the delta stream    Simple8b words or huff0, whichever is smaller
done stop repeating the block row   the group directory and the shared clock hold it
done exceptions in the scaled path  the engine's grid keeps a residual per value
gone one object for many series     groups and shared clocks took the row's bytes
P1   order-preserving integers      a second difference lost on TSBS; floats decide
P2   a summary shaped by kind       two columns of waste, after the corpus says
P2   a fixed width beside simple8b  0.26 bits, and it unblocks ordered integers
P3   Chimp, ALP-RD                  against a bound our noisy fixture is already near
```

The bar has moved twice. `≤1.0 bytes a sample` on dense whole numbers was taken
by deleting two indexes, which is not a compression result; `0.72` came from the
codec and the row together, and the payload is now at the order-0 entropy of its
own delta alphabet. The threshold worth defending is **0.7 bytes a sample over a
real mixed corpus**, measured whole, against another engine keeping the same
samples exactly — and on our own fixtures two classes of four are still above
it, by 0.02 and 0.046.
