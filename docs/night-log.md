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
