# TinyStore

An embedded time-series store for Go, on SQLite. Every sample is kept bit for
bit and every range is answered exactly. It is written for one machine watching
tens of thousands of series, not for a cluster, and it exists because
[Dashbin](https://github.com/tinyshed/dashbin) needs somewhere to put metrics
that does not cost more memory than the thing it is watching.

**The first embedded metrics slice works.** `metrics.Open` provides atomic
ingestion, label matching, exact range reads, explicit bounded maintenance and
reopen. The API is unreleased. See [the walkthrough](metrics/README.md) and its
[runnable example](metrics/example_test.go); Records, KV and SQL helpers remain
future work.

## Where a sample goes

```text
                 samples arrive
                       │
                       ▼
          ┌─────────────────────────┐
          │  recent and changeable  │   a late sample may still land here
          │          tail           │
          └────────────┬────────────┘
                       │  old enough that nothing may change it any more
                       ▼
          ┌─────────────────────────┐
          │          codec          │   240 samples, one compact byte string
          └────────────┬────────────┘
                       ▼
          ┌─────────────────────────┐
          │    immutable segment    │   written once, deleted whole
          └────────────┬────────────┘
                       ▼
                    SQLite
```

The durable head is a bounded packed tail per series. Sealing writes groups
of independent microblocks with shared clocks, constant/change/grid value
representations and inline or separate payloads. Reads combine head and groups
from one snapshot. Maintenance is called by the embedding application.

## What the codec does

It reads the samples before choosing how to write them, and keeps the smallest
result:

```text
   240 samples
        │
        ├── timestamps ──┬── every step equal      →  start, step, count
        │                ├── nearly equal          →  delta of delta
        │                └── arbitrary             →  delta
        │
        └── values ──────┬── all the same          →  nothing at all
                         ├── exact integers        →  delta, zigzag, simple8b
                         ├── written as decimals   →  a scale, then the same
                         ├── float, moving smoothly →  xor against the last
                         └── anything else         →  raw, eight bytes each
                                   │
                                   ▼
                        zstd, kept only when smaller
```

Nothing is rounded, scaled or approximated on the way in. `-0`, NaN payloads
and infinities come back as the bits that went in.

| 240 samples of         | bytes a sample |
|------------------------|----------------|
| one repeated value     | 0.033          |
| tenths of a degree     | 0.254          |
| whole numbers, walking | 0.456          |
| a counter              | 0.507          |
| a smooth float         | 7.838          |
| random IEEE-754 bits   | 8.000          |

Payload only. A whole SQLite file holding those blocks costs 0.72 bytes a
sample for the whole numbers and 0.52 for the decimals, with the summaries,
the indexes and the pages they sit in counted. The environment, the fixtures
and the command that reproduces each number are in
[docs/measurements.md](docs/measurements.md).

## Layout

| Path     | What it is                                                      |
|----------|-----------------------------------------------------------------|
| `codec/` | the block codec: what a payload looks like and how to read one  |
| `metrics/` | the durable metrics engine and its lifecycle tests           |
| `internal/sqlite/` | file handles, transactions and checked migrations      |
| `spike/` | prototypes and measurements, skipped unless `TINYSTORE_SPIKE=1` |
| `docs/`  | the design, the format, the numbers and what is still unknown   |
| `tools/` | a second module pinning developer tools                         |

## License

Apache-2.0.
