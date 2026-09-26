# TinyStore

An experimental embedded time-series store for Go, on SQLite. Every sample is
kept bit for bit and every range is answered exactly. It is written for one
machine watching tens of thousands of series, not for a cluster, and it was
built in the first place to give [Dashbin](https://github.com/tinyshed/dashbin)
somewhere to put telemetry that does not cost more memory than the thing it is
watching.

**Experimental and unreleased: the API moves without notice.** The metrics
engine works: atomic ingestion, label matching, exact range reads and streams,
exact aggregates over raw samples, bounded maintenance with per-series
quarantine, and reopen. See [the walkthrough](metrics/README.md) and its
[runnable example](metrics/example_test.go). Around it the store holds the
application's own SQL databases ([sqldb](sqldb/README.md)), its logs and events
([records](records/README.md)), its current state ([kv](kv/README.md)) and
backups; blobs, jobs and self-metrics are
designed in [docs/architecture.md](docs/architecture.md) and not built yet.

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
| `records/` | logs and events: a head, event-time segments, paged reads    |
| `kv/`    | the application's current state: typed buckets by key, versions |
| `sqldb/` | the application's own SQL databases in the store               |
| `backup/` | every engine's file in one checked zip, and its restore       |
| `examples/` | programs using the public API, built and tested with it     |
| `internal/sqlite/` | file handles, transactions and checked migrations      |
| `internal/admission/` | the gate and slots every engine lets work in through |
| `spike/` | prototypes and measurements, skipped unless `TINYSTORE_SPIKE=1` |
| `docs/`  | the design, the format, the numbers and what is still unknown   |
| `bench/` | a module of its own: corpus runners and the engines compared to |
| `tools/` | a second module pinning developer tools                         |

## Documentation

| | |
|---|---|
| [docs/design.md](docs/design.md) | how the store is meant to work, and why that shape |
| [docs/format.md](docs/format.md) | the bytes: the payload's layout, version by version |
| [docs/measurements.md](docs/measurements.md) | every number, its environment, and how to reproduce it |
| [docs/research.md](docs/research.md) | what is not built: the open questions and their gates |
| [docs/reports/](docs/reports/README.md) | the dated measurement rounds each number came from |

## License

Apache-2.0.
