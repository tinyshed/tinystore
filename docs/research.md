# What is not built

Nothing in this file works. Each entry is a hypothesis, an arithmetic estimate
or an open question, and each one says what it would have to show to be worth
writing. Where a number here is arithmetic over two measured numbers rather
than a measurement, it says so; the measured ones are in
[measurements.md](measurements.md).

## The physical shape of the head

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

## Where the rest of a dense block's bytes are

Two measured numbers, one subtraction:

```text
payload      0.671 B/sample × 240 = 161 B a block
whole file   1.101 B/sample × 240 = 264 B a block
                                  ───────────────
overhead                            103 B a block
```

Reaching 1.00 bytes a sample means taking 24 bytes off a block. The obvious
place to look is not the codec — it is the summary row, whose six floating
columns are up to 48 bytes beside a payload of 161. Whether they really cost 48
depends on something we have not checked: SQLite can store an integral value in
a column of real affinity as an integer, and whether it still does so in a
`STRICT` table decides whether those columns cost 48 bytes or nearer 12.

So the cheap first experiment is a decomposition rather than another codec: the
same ten thousand blocks, with one component removed at a time — the summary
columns, the expiry index, the series index, the separate payload row — and the
file measured after each. That says where the 103 bytes are instead of guessing
at them. A payload-to-page sweep comes second: it shows the packing cliffs, but
it cannot say what a row is made of.

## Exact decimals

The clearest hole the codec has. On tenths of a degree it is *worse* than the
generic encoding it replaced, 1.792 against 1.719 bytes a sample, because
nothing in it notices that

```text
21.3  21.4  21.4  21.5      is      213  214  214  215   at 10^1
```

Arithmetic, not a measurement: 239 zigzag deltas of 0, 1 or 2 fit two bits, 30
to a Simple8b word, so 8 words are 64 bytes; with the first value, a fixed-step
timestamp stream and the envelope that is about 100 bytes, or 0.42 bytes a
sample against 1.792. Four times, on a shape that real gauges have constantly.

It is only lossless if the check is per value and the decoder is the check:

```text
encode    m = int64(v * 10^k)
verify    float64(m) * 10^-k  is bit for bit v,  for every sample
decode    exactly the expression the verification used
```

Two traps. Go permits an implementation to fuse a multiply and an add into one
operation, so a fused expression can round differently on different
architectures; a bare multiply cannot, and the round trip has to be pinned by a
test on all three CI platforms. And `10^k` is exactly representable as a
`float64` only up to `k = 22`, so a candidate beyond that refuses. `-0`, NaN
and the infinities fail the check and fall back, which is the behaviour we
want rather than a special case to write.

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

And one honest caveat: the smooth signal is our own generator. Real monitoring
is mostly decimals and whole numbers, where scaled integers win and this does
nothing. Validate it on a real corpus before writing it.

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

## Entropy coding

Measure the entropy of the residual stream before reaching for a range coder.
If the residuals are 90% zero, 5% `+1`, 4% `-1` and 1% other, then

```text
H = 0.605 bits a value
Simple8b at width 1 = 1 bit + the selector ≈ 1.07
```

so the entire prize is 0.45 bits, or 0.056 bytes a sample — about 5% of a file
at 1.1 bytes a sample, in exchange for an entropy coder and its tables. A
cheaper thing to check first is our own selector table: selectors 0 and 1 spend
themselves on runs of *ones*, but after zigzag the common run is *zeros*, and
60 zeros currently cost a full 8-byte word. A zero-run selector would make 240
of them cost eight bytes. Check what the fixtures actually contain before
changing the table — a released format version cannot move.

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

The first version leaves out five-minute buckets, the automatic choice between
resolutions, the coarse retention tiers, `rate` and `increase`, the scheduler
and every materialisation. Its raw payload therefore stays until the block is
deleted. It may not leave out the foundation those features sit on: one ingest
path that takes any number of samples at any timestamps, the block builder, the
codec in both directions, the series registry with its postings and its
cardinality guard, and whether a series is a gauge or a counter. A first version
without those is not a first slice of this engine — it is a second engine,
shaped by whatever arrived first, with production data already on top of it by
the time the real one is written.

A counter is why the block summary is not only `count/min/max/sum`. An increase
over an hour is `last − first`, so both belong in the row, and a counter that
reset inside the hour has to be noticed while the block is built. Keeping raw
does not make the summary optional: a whole-block query should not decode it,
and a future coarse tier must not change the answer.

Events and metrics take a file each:

```text
data/
├── dashbin.db     sources, queries, dashboards, accounts
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
P0   decompose the 103 bytes        24 of them decide ≤1.0 B/sample
P0   exact decimals                 a measured hole, arithmetic says ×4
P0   the packed tail                the largest sparse prize, and the riskiest
P1   order-preserving integers      after a real corpus says smooth floats exist
P1   a real corpus                  without it, "we beat X" means nothing
P2   one object for many series     a layout question, in the sparse table
P2   zero runs, Rice                cheap, after predictors
P3   Chimp, entropy coding          only against a measured entropy gap
```
