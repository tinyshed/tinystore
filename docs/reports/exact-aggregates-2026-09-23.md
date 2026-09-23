# Exact raw-decoding aggregates and counter transitions

`metrics.Store.Aggregate` implements the [numerical contract](../aggregate-contract.md)
without trusting existing float64 directory summaries. One snapshot supplies
blocks and head, then decode and accumulation happen after the read transaction
ends. `AggregateRequest` selects `count`, `sum`, `min`, `max` or counter
`increase` and a positive whole-millisecond bucket width. Output buckets are
anchored at the requested `Range.From`; retention clips samples before they
contribute. `OutputSamples` limits aggregate buckets, while `DecodedSamples`,
payload bytes, blocks and series retain their separate bounds. An error returns
no aggregate results.

Every finite float64 is converted to an exact signed integer number of
`2^-1074` units. Gauge sums and counter increases add those integers and round
once to nearest-even float64. A finite result that rounds to infinity carries
`Overflow=true`; intermediate overflow can cancel without losing the answer.
Counter resets use each next observed sample, including the transition between
a sealed block and the head. The first sample of a bucket contributes zero,
and no transition crosses a bucket boundary. Nonfinite gauges and invalid
counter observations fail with typed errors. Raw values remain bit-exact and
readable independently of aggregation.

## Evidence and environment

Ryzen 7 7700, Windows 11, Docker Desktop 29.6.2 Linux amd64, 16 visible CPUs,
Go 1.27.1, modernc SQLite 1.59.0. Engine source is revision `4589144` on
`codex/architecture-measurements`, with
the benchmark harness in `dd4a6e5`. Tests and the timed read stage were
repeated after those commits. Unit and public-store tests cover:

- `1e16, 1, -1e16, 1` across two sealed blocks returns exactly 2;
- a counter reset across a sealed block/head boundary contributes the new
  first value, while two requested buckets do not share that transition;
- retention clipping inside a sealed block, signed-zero min/max, subnormals,
  intermediate overflow followed by cancellation and final overflow;
- nonfinite or negative input errors, a reopened counter file, output-bucket
  limits, signed timestamp extremes and 100 deterministic random vectors
  checked against an independent `big.Rat` rounding oracle.

The public corpus integration test reopened both complete normalized corpora,
read every sample bit for bit, and additionally compared the first 64 series'
aggregate sums and counts against `big.Rat`. TSBS had 2,020 series and
5,090,400 samples, JSONL SHA-256
`e4f502af7b0b2ff2c4dba92057a8f2b95e636882f3cb9900986e013d189572cf`.
Telegraf had 4,435 series and 4,939,641 samples, SHA-256
`6475b9d3dcf8cf2ce88de5084ea37981d9d5676cdf68086b89115fb1d808ad0d`.
Both 64-series aggregate checks passed. The existing `Read` still returns no
partial result; `Aggregate` has the same all-or-error form.

On the same 10,000-series × 500-sample fixture used by the architecture read
rounds, one-reader five-second ABBA measurements were:

| Shape | Exact Aggregate QPS | p50 | Peak sampled OS RSS | Input / output per query |
|---|---:|---:|---:|---|
| One series, one hour | 10,897; 10,965 | 70.9; 70.8 µs | 23.4; 23.2 MiB | 360 samples / 1 bucket |
| Region, one hour | 11.3; 11.2 | 88.1; 89.2 ms | 31.0; 30.9 MiB | 900,000 samples / 2,500 buckets |

The exact path decodes raw even for a complete sealed block. Its wide
throughput and RSS are a baseline for a future versioned summary shortcut,
not evidence that the legacy float64 sums may answer this API. The fixture
lived on Docker volume `tsperf`; queries were warm and ran one stage at a time.

## Reproduce and remaining format work

```sh
go test ./metrics -run '^TestAggregate|^TestExactAccumulatorMatchesRationalOracle$' -count=1
TINYSTORE_JSONL=<corpus>/series.jsonl go test ./metrics -run '^TestCorpusThroughPublicStore$' -v -count=1
<perf> -dir <volume>/arch-s10k -stage aggregate -shape hour -series 10000 -readers 1 -seconds 5
<perf> -dir <volume>/arch-s10k -stage aggregate -shape region -series 10000 -readers 1 -seconds 5
```

Build `<perf>` from `bench/perf` using Go 1.27.1 and the same engine source.
`task check` passed on Windows; the full Linux `go test -race -shuffle=on ./...`
passed. Darwin amd64 and arm64 CGO-free cross-builds passed, but native macOS
runtime and race tests did not run because the GitHub Actions quota was
exhausted; both Darwin metrics test binaries compiled but were not executed.

The [representation spike](aggregate-representation-2026-09-23.md) chose a
variable exact integer for a candidate directory version. That version,
golden old/new readers and summary execution are still unbuilt. A shortcut
must reproduce this raw-decoding API on clipped edges and whole blocks before
it can replace any decode.
