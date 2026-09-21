# The night of 21 September

A running log of one autonomous session: what was tried, what it measured, and
what was kept. Survivors move into [measurements.md](measurements.md) and
[research.md](research.md); this file keeps the failures too, because the
expensive thing is repeating them.

The goal set for the night: **0.70 bytes a sample over the whole SQLite file**
on dense structured data, with no debts. The rules, written down before any
measurement so they could not be bent afterwards:

```text
counted        the whole file after a checkpoint
included       blocks, payloads, summaries, every index a running store needs,
               and whatever metadata retention needs to find its work
not a win      a payload-only number
not a win      an index removed without its replacement measured
not a win      anything lossy, or a retention or query rule quietly relaxed
not a win      a summary dropped without an equally cheap query path
not a win      a bigger block whose query latency was not measured
```

Four classes are measured apart, because one average hides which one is
expensive: an integer walk, a counter, a decimal gauge, and a full-precision
float.

## Before anything: what a float block can possibly cost

`TestHowMuchOfAFloatBlockIsActuallyShared`, 64 blocks of 240.

| values          | bits shared by the whole block | neighbour XOR window | floor          |
|-----------------|--------------------------------|----------------------|----------------|
| noisy float     | 12.8                           | 44.0                 | 6.404 B/sample |
| sine            | 0.0                            | 48.5                 | 7.875          |
| random bits     | 0.0                            | 62.0                 | 8.000          |
| tenths, as bits | 13.0                           | 28.8                 | 6.375          |

We write 6.595 for the noisy walk against a floor of 6.404. So every
XOR-family idea — Chimp, Chimp128, ALP-RD — is arguing over three percent on
this data, and the class is not where a night should be spent. That is a
measurement on our fixture and not a statement about a real corpus, where a
"noisy" float often turns out to be a decimal and falls to the scaled path
instead.

## The baseline, with the file divided four ways

`TestTonightsBaseline`, 10 000 blocks of 240 samples per class, 1 000 series,
today's codec and today's schema, measured on Windows after a checkpoint.
`dbstat` gives each b-tree's pages, what they hold and what they waste, so
"waste" below is unused bytes inside pages that were paid for.

| class       | total | payload | metadata | indexes | page waste | summary scan | point |
|-------------|-------|---------|----------|---------|------------|--------------|-------|
| integers    | 1.147 | 0.670   | 0.249    | 0.158   | 0.070      | 780 us       | 26 us |
| counter     | 1.258 | 0.808   | 0.241    | 0.158   | 0.051      | 800 us       | 25 us |
| decimal     | 0.865 | 0.379   | 0.269    | 0.158   | 0.060      | 783 us       | 26 us |
| noisy float | 9.119 | 6.595   | 0.422    | 0.158   | 1.944      | 825 us       | —     |
| mixed       | 3.097 | 2.113   | 0.295    | 0.158   | 0.531      |              |       |

The surprise is the last column of the float row. A noisy block encodes to
about 1583 bytes, two of them fit a 4 KiB page, and the rest of the page is
bought and unused: **1.944 bytes a sample of nothing at all**, which is larger
than every index and every summary in the table put together. It is also the
one line here that has nothing to do with compression.

The three structured classes waste 0.05 to 0.07, so their road to 0.70 runs
through the payload, the summary row and the two indexes instead.

## Huffman over the delta stream

The integer and the scaled paths both end in ZigZag deltas, and both now write
them either as Simple8b words or through `huff0`, whichever comes out smaller.
`huff0` is a package of `klauspost/compress`, already in the module for zstd,
so this cost no dependency.

| 240-sample payload  | before | after | change |
|---------------------|--------|-------|--------|
| integer walk        | 0.671  | 0.564 | -16%   |
| integer counter     | 0.808  | 0.615 | -24%   |
| temperature, tenths | 0.379  | 0.362 | -4.5%  |
| integers, jittered  | 2.590  | 2.485 | -4%    |

In the whole file, same schema as the baseline:

| class       | before | after |
|-------------|--------|-------|
| integers    | 1.147  | 1.024 |
| counter     | 1.258  | 1.087 |
| decimal     | 0.865  | 0.847 |
| noisy float | 9.119  | 9.119 |
| mixed       | 3.097  | 3.019 |

What it costs, on the benchmark rather than the spike's coarse timer: encoding
a 240-sample integer block goes from 25.4 to 27.1 microseconds, decoding from
2.19 to 3.32. The summary scan is untouched, because it never decodes a
payload. Decoding half a microsecond slower per block for a sixth of the
payload is a trade worth making, and it is recorded here rather than hidden.

The first attempt was four times slower to decode, not fifty percent: `huff0`'s
`Scratch` is a large struct and I was allocating one per block. Holding one on
the codec, under the same mutex the rest of it uses, was the whole difference.

**The format moved for this.** A sub-encoding byte now says how the deltas were
written, so version 1 payloads written before tonight no longer read. That was
allowed exactly once: nothing has ever persisted a payload, there is no tag and
no store. From the first release the rule in `AGENTS.md` applies and this
becomes a version 2.

## The retention debt, settled

`TestWhatRetentionCostsWhenItWalksSeries`. Both paths carry `series_state`,
because a store needs the frontier however it finds its due work, so what
differs is only the index beside it. The sweep deletes every block whose last
sample is behind a cutoff **and its payload**, which the first version of this
measurement forgot to do and was flattered by.

Dense, ten blocks a series:

| schema                                          | file      | sweep a block |
|-------------------------------------------------|-----------|---------------|
| as it stands today                              | 1.038     | 12.0 us       |
| without the foreign-key index, foreign key kept | 0.956     | 678 us        |
| and retention that walks series, key kept       | 0.864     | 677 us        |
| no foreign key, retention walks series          | **0.864** | **6.4 us**    |

Sparse, one block a series:

| schema                                 | file      | sweep a block |
|----------------------------------------|-----------|---------------|
| as it stands today                     | 1.154     | 8.6 us        |
| without the foreign-key index          | 1.072     | 667 us        |
| and retention that walks series        | 1.038     | 686 us        |
| no foreign key, retention walks series | **1.038** | **21.8 us**   |

**Dropping the foreign key's index while keeping the foreign key is a
catastrophe, not a saving.** Deleting a payload makes SQLite look for the
blocks pointing at it, and without the index that is a scan of the whole block
table — a hundredfold slower sweep, 678 microseconds a block instead of six.
The index is not decoration; it is what makes the constraint usable. So the
choice is not "index or no index", it is "the database enforces this or the
transaction does".

Taking the key out and deleting both objects in one transaction gives the same
file and a sweep **faster than today's** on dense data, 6.4 against 12.0
microseconds a block. On sparse data it is 21.8 against 8.6, slower but still a
tenth of a second for five thousand blocks.

The global expiry index costs 0.1007 bytes a sample and the per-series index
that replaces it costs 0.0085 — one entry a series against one a block. On
sparse data, where a series has one block, that collapses to 0.0683 against
0.1024 and the structural win is mostly gone, which is the shape the packed
tail is for anyway.

What this buys, and what it costs:

```text
integers      1.024 → 0.864 bytes a sample, with a faster retention pass
the price     the database no longer enforces that a payload has a block
the gate      a deleted block leaves no payload, and no payload outlives a block
```

## Stop paying twice for what the row already holds

A block lives in a row that already carries its first timestamp as the primary
key, its last as `end_ts`, its sample count and its first value as `first`. The
body used to repeat all four. Now it repeats none of them:

```text
the head       start, end, count, first — returned by Encode, given back to Decode
fixed step     derived from (end - start) / (count - 1), so the whole timestamp
               stream of an evenly spaced block is now empty
the envelope   twelve header bytes down to four: a version, one byte of modes,
               and the length of the timestamp stream
the checksum   now covers the head as well as the body
```

That last line is what makes the trade safe. Moving the first value into a
column would otherwise mean a corrupted column silently changes every sample in
the block; checksumming the head with the body catches it, and a test feeds
`Decode` four heads that do not belong to the body to prove it. The last
timestamp must also equal `end_ts`, which is a second free check.

Payloads, 240 samples:

| workload            | before | after | against the original encoder |
|---------------------|--------|-------|------------------------------|
| one repeated value  | 0.142  | 0.033 | 0.171                        |
| integer walk        | 0.564  | 0.456 | 1.266                        |
| integer counter     | 0.615  | 0.507 | 1.424                        |
| temperature, tenths | 0.362  | 0.254 | 1.719                        |
| noisy float         | 6.644  | 6.536 | 7.415                        |

Twenty-six bytes a block, everywhere, for no new algorithm.

## Where the night stands after three changes

The whole file, ten thousand blocks a class, with the codec as it now is and
the schema the retention measurement chose — no foreign key, retention that
walks series:

| class       | baseline | tonight   | payload | metadata | indexes | waste |
|-------------|----------|-----------|---------|----------|---------|-------|
| integers    | 1.147    | **0.756** | 0.455   | 0.244    | 0.017   | 0.039 |
| counter     | 1.258    | **0.795** | 0.507   | 0.236    | 0.017   | 0.035 |
| decimal     | 0.865    | **0.573** | 0.254   | 0.268    | 0.017   | 0.034 |
| noisy float | 9.119    | 8.958     | 6.486   | 0.422    | 0.017   | 2.032 |
| mixed       | 3.097    | 2.771     | 1.926   | 0.293    | 0.017   | 0.535 |

The decimal class is under the night's target. The other two structured classes
are 0.056 and 0.095 above it, and the payload is no longer where the remaining
bytes are: for an integer walk it is 0.455 of payload against 0.300 of
everything else.

## What a block size costs a query

`TestWhatABlockSizeCostsAQuery`, 2.4 million samples a case, tonight's schema.

| samples a block | integers | payload | metadata | decode a block | a quarter hour decodes |
|-----------------|----------|---------|----------|----------------|------------------------|
| 30              | 2.877    | 0.832   | 1.854    | 1.0 us         | 1x what it answers     |
| 60              | 1.674    | 0.626   | 0.935    | ~1 us          | 2x                     |
| 120             | 1.067    | 0.512   | 0.479    | 2.0 us         | 4x                     |
| 240             | 0.756    | 0.455   | 0.244    | 4.0 us         | 8x                     |

Metadata halves with every doubling, exactly as arithmetic says it must, and
the payload improves too because the envelope and the Huffman table amortise.
Extrapolating one more step puts an integer walk near **0.57** at 480 samples a
block — under the night's target, and the reason I did not take it:

```text
480 samples a block would need the decode ceiling raised from 8 to 16 KiB
a narrow query would decode sixteen times what it answers, against eight
a block is deleted whole, so retention gets coarser
```

Two of those are exactly the rules this night was given. The lever is real, it
is priced, and it is somebody's decision rather than a number to quietly bank.

## What is left in the block row

Two squeezes that change no semantics: a gauge has no `increase` or `resets`
and a counter no `min`, `max` or `sum`, so those columns are null rather than
zero; and the row stores the span to its last sample rather than the absolute
timestamp, which is a smaller integer.

| class       | tonight | unused columns null | a span, not an end | both      |
|-------------|---------|---------------------|--------------------|-----------|
| integers    | 0.756   | 0.739               | 0.739              | **0.727** |
| counter     | 0.795   | 0.761               | 0.782              | **0.746** |
| decimal     | 0.573   | 0.534               | 0.563              | **0.521** |
| noisy float | 8.958   | 8.917               | 8.946              | **8.909** |

## The page size, and the biggest single win of the night

A page is bought whole. A noisy block encodes to about 1557 bytes, two of them
fit a 4 KiB page, and the remaining 962 bytes are paid for and empty.

| class             | 4 KiB     | 8 KiB     | 16 KiB | 32 KiB |
|-------------------|-----------|-----------|--------|--------|
| integers          | 0.727     | **0.720** | 0.744  | 0.778  |
| counter           | **0.746** | 0.761     | 0.758  | 0.778  |
| decimal           | **0.521** | 0.532     | 0.532  | 0.560  |
| noisy float       | 8.909     | **7.281** | 7.216  | 7.059  |
| the four averaged | 2.726     | **2.324** | 2.312  | 2.294  |

Eight kibibytes takes the float class from 8.909 to 7.281 — its page waste
falls from 2.033 bytes a sample to 0.426 — and moves the three structured
classes by less than two percent either way. On the mixed workload that is
fifteen percent of the whole file for one pragma.

It is not free and the cost was not measured tonight: a page is also the unit
the write-ahead log writes, so a dirty page costs eight kibibytes instead of
four. That measurement has to happen before this is adopted, and it is the
first thing on the list below.

The first attempt at this sweep reported that the page size changed nothing at
all, because `_pragma=page_size` in the connection string never took effect —
the file was 4 KiB in every case. A page size has to be set before the file
exists and before WAL, as its own statement, and the measurement only became
real once it printed the size the database actually had.

## Where the night ended

| class             | at the start | tonight, 4 KiB pages | tonight, 8 KiB pages | the target |
|-------------------|--------------|----------------------|----------------------|------------|
| integers          | 1.147        | 0.727                | **0.720**            | 0.70       |
| counter           | 1.258        | **0.746**            | 0.761                | 0.70       |
| decimal           | 0.865        | **0.521**            | 0.532                | 0.70 ✓     |
| noisy float       | 9.119        | 8.909                | **7.281**            | —          |
| the four averaged | 3.097        | 2.726                | **2.324**            |            |

**The target was missed on two classes of four and beaten on one.** An integer
walk is 0.020 over it and a counter 0.046; a decimal gauge is 0.18 under. The
float class was never a candidate: its floor, measured at the top of this file,
is 6.404 bytes a sample against the 6.486 we write, so it is within one and a
half percent of what any lossless encoding can do with those bits.

Where an integer walk's 0.720 sits:

```text
0.455  payload     already at the order-0 entropy of its delta alphabet:
                   3.38 bits a delta written, 3.36 measured for huff0, 3.17 the bound
0.213  metadata    51 bytes of SQLite row: the key, the span, the count,
                   five summary columns, the payload reference and the record header
0.017  indexes     one entry a series
0.034  page waste
```

So the payload is done until somebody finds a better model than "the previous
value", and that is what the corpus and the analyzer are for. Everything left
is the row, and the two levers on the row are both priced above: a bigger block
halves it and costs query latency and the decode ceiling; a page size moves it
by a couple of percent and costs write-ahead log volume.

What I would do next, in order:

```text
1  measure the write-ahead log at 4 and 8 KiB pages, on the same batches
   — one pragma is worth fifteen percent of a mixed file, and its only
     unmeasured cost is there
2  a corpus and the analyzer, because every remaining codec idea is now
   arguing about a model of the data we have never actually looked at
3  the block size decision, deliberately, with a range-query benchmark rather
   than the arithmetic above
4  the packed tail, which is the sparse story and untouched tonight
```

And two things this night refused to do, recorded so nobody has to re-derive
them: a bigger block was not taken silently, and no summary was dropped that a
query would have needed. The only semantic change made at all was that a gauge
now stores null where it used to store a zero it did not mean.
