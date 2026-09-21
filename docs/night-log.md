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
