# What is left in TSBS values, measured

A research round on one question: after the head, the label dictionary and the
shared clocks took the structural overhead out, is there another jump left in
the values themselves? Nothing here is built. No engine file was produced and no
production code was changed; every number below is a byte count from
`spike/`, where the whole round lives.

Four ideas went in. Two died, one is worth four percent, and one is worth
forty-three — and it is not a codec.

## Environment and corpus

Ryzen 7 7700, Windows 11, Go 1.27.1 windows/amd64, klauspost/compress 1.19.0,
working tree of `9e34181` plus the two uncommitted codec short-circuits, which
change no encoding choice. The figures are deterministic byte counts, so they do
not depend on the host; **no timing is claimed in this document** and none of
this ran in the usual linux container.

The corpus is the pinned TSBS DevOps set of
[the packed-head run](packed-head-2026-09-21.md), verified by checksum before
use: 2020 series, 5090400 samples, 101 fields across nine measurements times
twenty hosts, ten-second cadence.

```text
e4f502af7b0b2ff2c4dba92057a8f2b95e636882f3cb9900986e013d189572cf  series.jsonl
```

The baseline is every sample in 240-sample blocks through the current codec:
**2921220 payload bytes, 0.5739 B/sample.** That is
[the TSBS run's](tsbs-2026-09-21.md) 0.5782 without its one-byte model wrapper,
so the two agree; the whole engine file at the last measurement was 5099520
bytes, 1.0018 B/sample.

## The payload is eight fields

A per-b-tree division of the file cannot say which *field* holds the bytes. Per
field, over every block:

| field | series | B/sample | share of payload |
|---|---:|---:|---:|
| `mem_used_percent` | 20 | 5.4144 | 9.34% |
| `mem_buffered_percent` | 20 | 5.4110 | 9.34% |
| `mem_available_percent` | 20 | 5.4098 | 9.33% |
| `mem_used` | 20 | 4.1386 | 7.14% |
| `mem_cached` | 20 | 4.1381 | 7.14% |
| `mem_available` | 20 | 4.1370 | 7.14% |
| `mem_free` | 20 | 4.1370 | 7.14% |
| `mem_buffered` | 20 | 4.1298 | 7.13% |
| the other 93 fields | 1860 | 0.2263 | 36.31% |

**Eight fields of 101 hold 63.7% of the payload on 7.9% of the samples.** The
other 93 are already at 0.2263 B/sample: counters whose deltas live in a nine-
to eleven-symbol alphabet, CPU gauges walking by ±3, and 80 series that are
constant. Nothing a new codec does to those 93 fields can move the corpus much.

The envelope is visible in the same run: encoding the same blocks with one
constant value costs 8 bytes a block, 0.0349 B/sample, and a fixed cadence
costs nothing at all. So 0.5739 is 0.5390 of values and 0.0349 of envelope.

## Those eight fields are three signals

Checked on bits over all 2520 samples of `host_0`:

```text
mem_used + mem_available == mem_total      2520 of 2520
mem_used + mem_free      == mem_total      2520 of 2520
mem_free                 == mem_available  every sample, every host
mem_used_percent      == mem_used/mem_total*100       exact, all four groupings
mem_available_percent == mem_available/mem_total*100  exact
mem_buffered_percent  == mem_buffered/mem_total*100   exact on 15 of 20 hosts
```

`mem_total` is constant. So of the ten `mem` fields, **three carry information**
— one walk shared by used/available/free, plus cached and buffered — and seven
are a constant, a copy, a complement or a ratio.

A census over every host and every measurement, accepting only derivations that
reproduce every bit:

| kind | series | payload | share |
|---|---:|---:|---:|
| independent | 1805 | 1736925 B | 59.46% |
| percent | 55 | 733097 B | 25.10% |
| complement | 60 | 235654 B | 8.07% |
| duplicate | 20 | 208504 B | 7.14% |
| constant | 80 | 7040 B | 0.24% |

**40.3% of the payload is in series that carry no information of their own.**

## The five hosts that disagree are the interesting ones

`mem_buffered_percent` is bit-exact on fifteen hosts and not on five. On those
five it is never off by more than **one unit in the last place**:

| host | exact | off by one ULP | distinct distances |
|---|---:|---:|---:|
| host_0 and fourteen others | 2520 | 0 | 1 |
| host_1 | 1166 | 1354 | 3 |
| host_6 | 1425 | 1095 | 3 |
| host_9 | 1156 | 1364 | 3 |
| host_14 | 1542 | 978 | 3 |
| host_15 | 1133 | 1387 | 3 |

This is the finding that turns a benchmark curiosity into a mechanism. A rule
that demands the producer's own expression captures fifteen hosts and gives up
on five. A rule that stores **a prediction plus the distance to it, counted in
units in the last place**, captures all twenty, is bit-exact by construction,
and never has to guess how the exporter associated its multiplication. The
correction stream for those five hosts is a three-symbol alphabet and costs
about 1.9 bits a sample, against the 43 bits a sample the field costs today.

It is the same shape as the rest of this store — a dominant model and its
exceptions — moved from the value domain into the bit domain.

## What that would weigh

Every field is offered every sibling in its own measurement under three
predictors (copy, complement of a constant, ratio to a constant), the cheapest
correction stream wins, and a predicted field is never itself used as a
prediction. The correction stream is packed with a Huffman table of its own and
charged the same 8-byte envelope a block pays today:

| | payload | B/sample |
|---|---:|---:|
| now | 2921220 | 0.5739 |
| predicted fields store only their correction | 1675091 | **0.3291** |

**42.7% of the payload, and 352800 samples were restored from their sibling and
compared bit for bit in the same run.** 140 series of 2020 stop holding values:

| kind | series | exact | was | becomes |
|---|---:|---:|---:|---:|
| complement | 60 | 60 | 235654 B | 5940 B |
| duplicate | 20 | 20 | 208504 B | 1980 B |
| percent | 60 | 55 | 818256 B | 8365 B |
| stored | 1880 | — | 1658806 B | 1658806 B |

The predictor search is greedy and its forest is not the optimal one: an earlier
non-deterministic ordering reached 0.3169 on the same corpus. 0.3291 is the
reproducible number, and the gap is a search problem rather than a format one.

## Two ideas that died

**Predicting one instance of a field from another does not work here.** Order-0
bits a sample over each whole field family, on the same 5090400 samples:

| stream | bits a sample |
|---|---:|
| own delta | **2.440** |
| own delta of delta | 2.676 |
| residual against host_0 at the same instant | 4.747 |
| the delta of that residual | 2.518 |

Subtracting a neighbour nearly doubles the cost, because it adds that
neighbour's noise to your own; mean correlation between one host's deltas and
host_0's is 0.359, which is not enough to pay for it. The second line answers a
separate proposal with the same measurement: **a second difference is already
worse than the first on this corpus**, which is the noise arithmetic in
[research.md](../research.md) now measured rather than argued.

**Sharing a compression context across a family does not work either.** The
same own-delta stream through five packers, value bytes a sample:

| packer | B/sample |
|---|---:|
| the current codec, values only | **0.5389** |
| zigzag varints | 1.3794 |
| huff0, a table per block | 0.9576 |
| huff0, one table per family | 0.9285 |
| zstd per block | 0.6969 |
| zstd, one stream per whole family, no random access | 0.6119 |

Even with every block of every host of a field in one compression context and
random access given up entirely, a shared context lands **above** what the
per-block codec already reaches. The transform-free version of the same check is
stronger: compress the concatenation of the actual codec bodies of each field
and the corpus goes from 2921220 to 2718130 bytes — **7.0% of redundancy left
between the blocks we already write**, and that includes their repeated
envelopes.

zstd's dictionary builder returned an empty dictionary on these inputs, so that
column is not a result. It cannot change the conclusion: a dictionary trained on
a family cannot beat the family in one stream, and that already loses.

## One idea worth four percent

A Huffman table shared by a field's instances, stored once and referenced, the
way a clock object already is. Taking the smaller of the current body and the
shared-table body, block by block, tables included:

```text
current                                      0.5389 value B/sample
current, or the shared table when smaller    0.5176   (−3.95%)
shared table chosen in 11652 of 22220 blocks, 1504 bytes of tables in total
```

Four percent of the payload for a refcounted shared object and a second lookup
on the read path. Real, small, and it competes for the same machinery as the
mechanism above.

## The other half of the file

The payload is 0.6043 of the engine's 1.0018 B/sample. Of the rest, the largest
addressable item is what a series costs before a sample is stored. Canonical
labels are still stored as text:

| | total | per series | B/sample |
|---|---:|---:|---:|
| canonical label text | 555448 B | 275.0 | 0.1091 |
| the same as sorted dictionary ids | 27423 B | **13.6** | 0.0054 |
| the dictionary behind them | 5952 B over 302 distinct pairs | | |

Twenty times smaller, and the identity invariant survives: a digest match still
resolves by comparing full canonical labels, because the id list reconstructs
them exactly. The label dictionary migration took this cost out of postings and
left it in the registry.

## The floor, and why 0.3 is not on this corpus

What an ideal coder would need for the same values:

| | B/sample |
|---|---:|
| payload now | 0.5739 |
| an ideal coder over own deltas | 0.3582 |
| the same, with derivable fields dropped | **0.2612** |

A histogram is only a bound while symbols repeat, so the five `mem` walks — 2442
distinct deltas in 2519 samples — are bounded by their widths instead, at
29.4 bits a sample against the 32.8 they cost today. Those walks are near-
uniform noise by construction and no predictor beats them.

Against that floor:

```text
values, ideal coder, derivations applied      0.2612
the mutable packed head, measured              0.0877
groups, measured                               0.0805
postings after the dictionary, measured        0.0491
                                              ──────
                                               0.4785   before one byte of identity
```

**0.3 B/sample over this corpus is below the information content of its own
values plus the structures the engine already has.** It is not a hard target, it
is an impossible one, unless bits are dropped — which this store does not do.
Anyone quoting 0.3 has to say which of those four lines they are deleting.

0.5 is a different matter, and reachable only if groups and the head are
attacked beside the values.

## What a longer corpus does, as arithmetic

Splitting the measured file by what scales with samples and what scales with
series:

```text
per sample   payloads 0.6043 + groups 0.0805 + other 0.0056   = 0.6904
per series   labels, identity, postings, due, state, dictionary
             784.7 B a series = 0.3113 here, 0.0030 over thirty days
```

This corpus is seven hours, 2520 samples a series. The same shape kept for
thirty days at the same cadence is 259200 samples a series, and the fixed part
falls about a hundredfold:

| | this corpus | thirty days, arithmetic |
|---|---:|---:|
| today | 1.0018 | 0.6934 |
| with predicted fields | — | 0.4356 |
| and shared tables | — | ~0.42 |

**This is arithmetic over measured components, not a run.** It assumes the same
field mix and cadence, and it ignores that thirty days of blocks would change
page packing. It is the honest version of "the fixed cost dissolves": it does,
and it leaves 0.42, not 0.3.

## What this does not establish

**TSBS is shaped like telegraf, and telegraf is where derived fields come
from.** `used + free == total`, `used_percent == used/total*100` and
`inodes_used == inodes_total - inodes_free` are identities a real exporter
computes, so the mechanism is not synthetic — but `mem_free == mem_available` is
a generator artifact, real Linux does not do that, and the magnitude here is
inflated further by the `mem` walks being the noisiest fields in the corpus.
**The 42.7% is this corpus' number. The gate before any of it is written is the
same measurement on a corpus nobody shaped for us** — the Alibaba trace, whose
archive is no longer on this machine, or an OpenTelemetry capture.

Nothing here was run through the engine. The projections are byte counts over
research blocks; a payload's size is not a file's size, page slack does not
scale linearly with a body, and the file-level lines above are arithmetic and
labelled as such.

The mechanism's bill has not been paid either, and it is not small: a block that
is decoded from another series' block needs a reference that retention cannot
break, ingest that delivers both fields in one batch, a decode budget of two
blocks instead of one, and a query planner that knows a series may be behind
another one. Clocks are already refcounted shared objects, so the shape exists —
but a clock is read-only arithmetic and a value reference is a dependency.

## Reproduce

```powershell
$env:TINYSTORE_SPIKE=1
$env:TINYSTORE_JSONL='<repo>/bench/corpus/tsbs-packed/series.jsonl'
go test ./spike -run '^TestValueBudgetByField$|^TestDerivableSeriesCensus$|^TestMemFieldStructure$|^TestPercentAcrossHosts$|^TestSharedModelsAgainstCrossSeriesPrediction$|^TestCorpusFloor$|^TestDerivedProjection$' -v -count=1
```

New files, all in `spike/` and all skipped without `TINYSTORE_SPIKE`:
`oracle_spike_test.go`, `structure_spike_test.go`, `census_spike_test.go`,
`shared_spike_test.go`, `floorplan_spike_test.go`, `projection_spike_test.go`.

`go test ./...`, `gofmt` and `golangci-lint run ./spike/...` pass on Windows.
`task check` was not run, no container run was made, no measurement of timing,
WAL, RSS or query latency was attempted, and no commit was made.
