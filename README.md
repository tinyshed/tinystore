# TinyStore

An embedded time-series store for Go, on SQLite. Every sample is kept bit for
bit and every range is answered exactly. It is written for one machine watching
tens of thousands of series, not for a cluster, and it exists because
[Dashbin](https://github.com/tinyshed/dashbin) needs somewhere to put metrics
that does not cost more memory than the thing it is watching.

**Nothing here is usable yet.** The block codec is implemented, tested and
measured. The store around it — ingest, compaction, retention, queries — is
not. There is no API to depend on, no release, and no compatibility promise.

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

Of those four boxes only the codec exists. The tail and the segment are a
design with measurements behind it, written down in
[docs/design.md](docs/design.md) and not yet implemented.

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
        └── values ──────┬── all the same          →  one bit pattern
                         ├── exact integers        →  delta, zigzag, simple8b
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
| one repeated value     | 0.142          |
| whole numbers, walking | 0.671          |
| a counter              | 0.808          |
| a smooth float         | 7.946          |
| random IEEE-754 bits   | 8.108          |

Payload only; a whole SQLite file costs more. The environment, the fixtures and
the command that reproduces each number are in
[docs/measurements.md](docs/measurements.md).

## Layout

| Path     | What it is                                                      |
|----------|-----------------------------------------------------------------|
| `codec/` | the block codec: what a payload looks like and how to read one  |
| `spike/` | prototypes and measurements, skipped unless `TINYSTORE_SPIKE=1` |
| `docs/`  | the design, the format, the numbers and what is still unknown   |
| `tools/` | a second module pinning developer tools                         |

## License

Apache-2.0.
