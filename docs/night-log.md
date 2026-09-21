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

| values         | bits shared by the whole block | neighbour XOR window | floor       |
|----------------|--------------------------------|----------------------|-------------|
| noisy float    | 12.8                           | 44.0                 | 6.404 B/sample |
| sine           | 0.0                            | 48.5                 | 7.875       |
| random bits    | 0.0                            | 62.0                 | 8.000       |
| tenths, as bits| 13.0                           | 28.8                 | 6.375       |

We write 6.595 for the noisy walk against a floor of 6.404. So every
XOR-family idea — Chimp, Chimp128, ALP-RD — is arguing over three percent on
this data, and the class is not where a night should be spent. That is a
measurement on our fixture and not a statement about a real corpus, where a
"noisy" float often turns out to be a decimal and falls to the scaled path
instead.
