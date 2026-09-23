# Exact aggregate representation and boundary contract

The proposed arithmetic is in [the aggregate contract](../aggregate-contract.md).
This round measured representations before the later
[raw-decoding aggregate API](exact-aggregates-2026-09-23.md). No directory
version or summary-driven query was implemented here. Existing float64
summaries cannot be used for the correctly rounded sum.

## Inputs and environment

The harness is `TestAggregateRepresentationSpike` in `spike/`. It partitions
each normalized JSONL series into consecutive groups of at most 240 values;
these groups are not the engine's final packed-block boundaries. Each float64
contributes an exact signed integer number of units of `2^-1074`. The harness
checks a floating expansion against that integer for every finite group.

Ryzen 7 7700, Windows 11, Docker Desktop 29.6.2, Linux amd64 container with
16 visible CPUs, Go 1.27.1. Spike source is saved as `6a0465e` on
`codex/architecture-measurements`; both corpus passes reproduced their byte
totals after that commit. TSBS normalized JSONL SHA-256:
`e4f502af7b0b2ff2c4dba92057a8f2b95e636882f3cb9900986e013d189572cf`.
The local Telegraf corpus SHA-256:
`6475b9d3dcf8cf2ce88de5084ea37981d9d5676cdf68086b89115fb1d808ad0d`.
The corpora contain 5,090,400 and 4,939,641 samples respectively.

## Result

| Corpus | Candidate groups | Variable exact integer | Fixed 272-byte accumulator | Floating expansion | Largest expansion |
|---|---:|---:|---:|---:|---:|
| TSBS | 22,220 | 130,978 B (5.90/group) | 6,043,840 B | 201,956 B (9.09/group) | 5 terms |
| Telegraf | 23,387 | 103,836 B (4.44/group) | 6,361,264 B | 229,179 B (9.80/group) | 4 terms |

The variable candidate encodes sign, a two-byte exponent allowance, and the
nonzero magnitude after removing trailing zero bits; zero takes one byte.
These counts exclude directory framing and SQLite pages and are not persisted
file measurements. Both corpora had only finite values. No expansion failed on
them, but the finite sequence `MaxFloat64, MaxFloat64, -MaxFloat64,
-MaxFloat64` overflows an intermediate expansion component while its exact sum
is zero. An ordinary expansion therefore needs a separate overflow path and a
proved size bound. The exact integer has a simple domain bound: a 240-sample
block needs at most 264 magnitude bytes, with a small sign/exponent header.

The variable exact integer is the candidate for a new directory version. It is
smaller than both alternatives on these inputs and handles intermediate
overflow and cancellation. The current directory's float64 `sum` and
`increase` remain readable but cannot satisfy the proposed exact aggregate
contract. Counter transitions between blocks still require the stored first
and last values and exact addition across boundaries.

## Reproduce

```sh
TINYSTORE_SPIKE=1 TINYSTORE_JSONL=<corpus>/series.jsonl \
  go test ./spike -run '^TestAggregateRepresentationSpike$' -v -count=1
```

Run once per named corpus and verify its SHA-256 first. The corpus is fetched
or prepared locally and is never committed.
