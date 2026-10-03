# Ingesting

Store samples that come from somewhere other than your own instruments: an
agent, a device, an import of old data. A sample is a time and a value, and
TinyStore keeps every bit of the value.

## Ingest samples

```ts
await store.metrics.ingest({
	name: 'cpu',
	kind: 'gauge',
	labels: { host: 'web-1' },
	samples: [
		[new Date('2026-10-03T09:00:00Z'), 0.42],
		[new Date('2026-10-03T09:00:15Z'), 0.47],
	],
})
```

```python
await store.metrics.ingest({
    "name": "cpu",
    "kind": "gauge",
    "labels": {"host": "web-1"},
    "samples": [(datetime(2026, 10, 3, 9, 0, tzinfo=UTC), 0.42), (datetime(2026, 10, 3, 9, 0, 15, tzinfo=UTC), 0.47)],
})
```

```go
err = stats.Ingest(ctx, []metrics.Batch{{
	Series: metrics.Series{Name: "cpu", Kind: metrics.Gauge, Labels: metrics.Labels{"host": "web-1"}},
	Samples: []metrics.Sample{
		{At: start.UnixMilli(), Value: 0.42},
		{At: start.Add(15 * time.Second).UnixMilli(), Value: 0.47},
	},
}})
```

A series is a name, a kind, `gauge` or `counter`, and its labels. Times are
unix milliseconds, or a `Date` or `datetime`. In Bun and Python, you can also
pass the samples as two columns, `times` and `values`.

One `ingest` call is atomic: every sample of every series is stored, or none
is. If one series is rejected, the error names it by its labels, so you can
send the call again without it.

## Samples out of order

Samples can arrive in any order, and a sample for a time that already has one
replaces it. Every series keeps track of the newest time it has seen. Samples
far behind it are packed into compressed blocks over time, and a sample older
than the series' packed blocks is rejected with `ErrTooOld`, because
changing a packed block would change a counter's increase.

How late a sample may arrive is the `Lateness` option, zero by default.
Lateness follows each series' own newest sample, not the clock, so an agent
that comes back after a day offline can still send its buffered samples.

## The time window

A sample must be newer than the retention, 30 days by default, and no more
than ten minutes ahead of the store's clock. Samples outside the window are
rejected with `ErrTooOld` or `ErrTooNew`. A device with a wrong clock can't
store samples in the future, where they would stay past the retention.

## Values are kept exactly

A value comes back with exactly the bits it was stored with: `-0`, infinities
and the payload of a `NaN` included. On the wire, values travel as raw bytes,
so even Bun and Python, which only work with float64 values, keep the
original bits.

Values are compressed as they are stored. Whole numbers and decimals with a
few digits take a fraction of a byte per sample.

## Remove a series

```ts
await store.metrics.drop({ name: 'cpu', labels: { host: 'web-1' } })
```

```python
found, unreadable = await store.metrics.drop("cpu", {"host": "web-1"})
```

```go
dropped, err := stats.DropSeries(ctx, "cpu", metrics.Labels{"host": "web-1"})
```

`drop` removes one series and all of its samples, even if its data can no
longer be read.

## Limits and defaults

| | |
|---|---|
| Samples per `ingest` | 10,000, or 4 MiB |
| Lateness | 0 |
| Retention | 30 days |
| Clock skew | 10 minutes |
| Series | 100,000 |

## See also

- [Instruments](instruments.md): measure your own program instead.
- [metrics/README.md](../../metrics/README.md): the full contract of ingest.
