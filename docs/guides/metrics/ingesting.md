# Ingesting

`ingest` writes samples a program has from elsewhere: a sensor's readings, a
host's CPU, a counter another system kept. Each series is a name, a kind and
its labels, with samples of a time and a number, kept bit for bit.

```ts
await store.metrics.ingest({
	name: 'cpu',
	kind: 'gauge',
	labels: { host: 'web-1' },
	samples: [[new Date('2026-10-11T09:00:00Z'), 0.42], [new Date('2026-10-11T09:00:15Z'), 0.47]],
})
await store.metrics.ingest([cpu, memory, disk]) // all or none
await store.metrics.ingest({ name: 'cpu', kind: 'gauge', labels, times, values }) // as two columns
await store.metrics.ingest({ name: 'signups_total', kind: 'counter', labels: {}, samples, keep: '730d' })
```

```python
await store.metrics.ingest(Series(name="cpu", kind="gauge", labels={"host": "web-1"}, samples=[(at, 0.42), (later, 0.47)]))
await store.metrics.ingest([cpu, memory, disk])  # all or none
await store.metrics.ingest(Series(name="cpu", kind="gauge", labels=labels, times=times, values=values))
await store.metrics.ingest(Series(name="signups_total", kind="counter", labels={}, samples=samples, keep="730d"))
```

```go
err := metrics.Ingest(ctx, store, metrics.Series{
	Name: "cpu", Kind: metrics.KindGauge, Labels: metrics.Labels{"host": "web-1"},
	Samples: []metrics.Sample{{At: at, Value: 0.42}, {At: later, Value: 0.47}},
})
err = metrics.Ingest(ctx, store, cpu, memory, disk) // all or none
```

```rust
let stats = store.metrics()?;
stats.ingest(&[Series::gauge("cpu").label("host", "web-1").sample(at, 0.42).sample(later, 0.47)])?;
```

- One call is written whole or not at all. A refused series names itself by
  its labels, so the call can be sent again without it.
- `ingest` returns once the samples are written, and a read finds them at
  once. A crash of the program loses none; a power loss may take the last
  ones, since the metrics' file commits as `os` unless the store opened
  `full`.
- `samples` takes pairs, and `times` with `values` take the same as two
  columns: unix milliseconds or dates, and numbers.

## Kinds

| `kind`      | For                                                                |
|-------------|--------------------------------------------------------------------|
| `gauge`     | a value that goes up and down                                      |
| `counter`   | a total that only grows; a restart is a reset                      |
| `histogram` | a `_bucket` series of a histogram written elsewhere, with its `le` |

A name has one kind while it has series: a gauge and a counter of one name is
`invalid`. An ingested series has one writer: two programs ingesting one
counter under the same labels write over each other's totals, so each gives it
a label of its own, `instance`. A counter that another program restarted is
sent with a zero at the time it started, as the instruments write one, so the
increase across the restart is right.

## Bit for bit

A sample is kept exactly: `-0`, infinities and a NaN's payload come back as
they went in. Every transform the store packs with proves the original bits
come back, or the block is kept another way. A time is kept to the
millisecond.

## Time

- Samples may come in any order. A time given twice keeps the later value,
  until the store has packed that stretch of the series.
- A sample behind what its series has packed is `too old`, since a packed
  counter's increase would change (NOT-08). A series that comes back after a
  day away sends its backlog: what it has packed follows its own newest
  sample, not the clock.
- A time is taken from as long ago as its name keeps to ten minutes ahead of
  the store's clock.

## A name's `keep`

A name's kind, `keep` and buckets are kept in the file with it: an instrument
and an `ingest` that give one set it for every series of the name. A name
nobody gave a `keep` keeps 30 days. While the store is open, giving another
is `invalid`, naming both, so one part of a program never shortens what
another keeps.

## From Prometheus

Prometheus' text format is not read by the store yet. A program that scrapes
an exporter parses the text with a library of its language and ingests each
series, a counter as a counter and a gauge as a gauge, with a label of the
host it scraped. A histogram's `_bucket` series go in as `histogram`, each
with its `le`, beside its `_sum` and `_count` counters.

## Limits

| What              | Limit                                                   |
|-------------------|---------------------------------------------------------|
| one `ingest`      | 10,000 samples or 4 MiB                                 |
| a series          | 128 labels with its name, 16 KiB; a label's value 4 KiB |
| a sample's time   | from `keep` ago to ten minutes ahead                    |
| series in a store | 100,000                                                 |

## See also

- [Metrics](README.md): the engine's overview.
- [Reading](reading.md): the samples back, bit for bit.
