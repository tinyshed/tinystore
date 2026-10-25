# Ingesting

`ingest` writes samples a program has from elsewhere: a sensor's readings, a
host's CPU, a counter another system kept. Each series is a name, a kind and
its labels, with samples of a time and a number, kept bit for bit.
`ingestPrometheus` writes the text a Prometheus exporter serves.

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
- After that, a sample with a new value behind what its series has packed
  fails with a too old error, and the whole call fails with it. A packed
  counter's increase would change otherwise (NOT-08). The same sample sent
  again is accepted, as [the next section](#an-ingest-sent-again) shows.
- A series that comes back after a day away sends its backlog: what it has
  packed follows its own newest sample, not the clock.
- A time is taken from as long ago as its name keeps to ten minutes ahead of
  the store's clock.

## An ingest sent again

A job step runs at least once. If a collector's worker crashes after
`ingest` and before its step finishes, the next attempt sends the same
samples again. That second call changes nothing, even after the store has
packed those samples:

```ts
await store.metrics.ingest(revenue) // three hours of samples, read from a database
await store.metrics.ingest(revenue) // the step's next attempt: nothing changes
```

```python
await store.metrics.ingest(revenue)  # three hours of samples, read from a database
await store.metrics.ingest(revenue)  # the step's next attempt: nothing changes
```

```go
err := metrics.Ingest(ctx, store, revenue) // three hours of samples, read from a database
err = metrics.Ingest(ctx, store, revenue)  // the step's next attempt: nothing changes
```

```rust
let stats = store.metrics()?;
stats.ingest(&[revenue.clone()])?; // three hours of samples, read from a database
stats.ingest(&[revenue])?; // the step's next attempt: nothing changes
```

- A sample counts as sent again when its time and its value's bits are the
  ones the store keeps. `0` and `-0` differ, and so do two NaNs with different
  payloads.
- Any other sample behind what its series has packed fails the whole call
  with a too old error (`TooOldError` in Bun and Python, `ErrTooOld` in Go,
  `ErrorKind::TooOld` in Rust). The error's `what` names the sample's time,
  `at`, and the time before which its series is packed, `packed`. To write
  the rest, send that series again without its samples before `packed`.
- A sample older than its name keeps is too old, sent again or not.
- A value that changed after the fact, such as yesterday's revenue after a
  refund, can't replace a packed sample.

## A name's `keep`

A name's kind, `keep` and buckets are kept in the file with it: an instrument
and an `ingest` that give one set it for every series of the name. A name
nobody gave a `keep` keeps 30 days. While the store is open, giving another
is `invalid`, naming both, so one part of a program never shortens what
another keeps.

## From Prometheus

`ingestPrometheus` reads the text that a Prometheus exporter serves, such as
node_exporter's `/metrics`, and writes all of its series in one call. It
reads Prometheus' text format and OpenMetrics. TinyStore doesn't fetch the
text: your program scrapes the exporter and passes the body in.

```ts
const text = await (await fetch('http://web-1:9100/metrics')).text()
await store.metrics.ingestPrometheus(text, { labels: { instance: 'web-1' } })
```

```python
text = httpx.get("http://web-1:9100/metrics").text
await store.metrics.ingest_prometheus(text, labels={"instance": "web-1"})
```

```go
resp, err := http.Get("http://web-1:9100/metrics")
body, err := io.ReadAll(resp.Body)
err = metrics.IngestPrometheus(ctx, store, body, metrics.Labels{"instance": "web-1"})
```

```rust
let stats = store.metrics()?;
let web = stats.prometheus().label("instance", "web-1");
web.ingest(&body)?; // body: the text of http://web-1:9100/metrics, from any HTTP client
```

Each sample line becomes a series with one sample, of the kind that its
`# TYPE` line gives it:

| `# TYPE`                             | Series                                                                                  |
|--------------------------------------|-----------------------------------------------------------------------------------------|
| `counter`                            | a `counter` named as the sample, such as `node_cpu_seconds_total`                       |
| `gauge`                              | a `gauge`                                                                               |
| `histogram`                          | each `_bucket` series as `histogram` with its `le`, `_sum` and `_count` as counters     |
| `summary`                            | a `gauge` per quantile with its `quantile` label, `_sum` and `_count` as counters       |
| `untyped`, `unknown` or none         | a `gauge`                                                                               |
| `info`, `stateset`, `gaugehistogram` | a `gauge` per sample line, such as `build_info` and each `_bucket` of a gauge histogram |

- `labels` adds labels to every series: `label` in Rust, `metrics.Labels` in
  Go. If the text already has one of them, the call fails with an invalid
  error that names the line, because one value would hide the other. Pick a
  label that the exporter doesn't use.
- A sample without a timestamp gets `at` (`metrics.At` in Go), or the store's
  clock when the call runs. Prometheus' timestamps are milliseconds. A text
  whose last line is `# EOF` is read as OpenMetrics, whose timestamps are
  seconds, rounded to the millisecond.
- `keep` sets how long every name of the text keeps its samples, as `keep`
  does on a series.
- Values are kept bit for bit, `NaN`, `+Inf`, `-Inf` and `-0` included. A
  label with an empty value is the same as no label, as in Prometheus.
- A name with characters other than letters, digits, `_` and `:`, such as
  OpenTelemetry's dots, comes in quotes in Prometheus 3's text:
  `{"http.server.request.duration", method="GET"} 0.25`. TinyStore keeps
  the name as it is, dots and all, and reads a quoted label name the same
  way. An exporter writes quoted names only when the scrape asks for them;
  otherwise it writes underscores in their place.
- A family's `# HELP` gives each of its names, such as `rpc_seconds_bucket`,
  `_sum` and `_count`, a help where the name has none. Its `# UNIT` goes to
  the names whose values are in that unit: `rpc_seconds_sum` takes
  `seconds`, while `_bucket` and `_count`, which count observations, take
  none.
- What `describe` set in your code always stays. Otherwise the first text
  that describes a name wins, and a later one changes nothing. A help past
  1,024 bytes or a unit past 32 is left out, and the samples are still
  written. OpenMetrics' exemplars are not kept.
- OpenMetrics' `_created` samples are left out, because a series doesn't keep
  the time that its counter started. A counter's reset counts its new value
  whole, so a restart still reads right. Only what a new counter held before
  its first scrape isn't counted.

The text is read whole before anything is written. If a line can't be read,
the call fails with an invalid error (`InvalidError`, `ErrInvalid`) that names
the line and quotes its start, and nothing is written:

```text
metrics: line 12, `node_load1{host="web-1} 0.42`: label host: a value without its closing quote
```

A series that the store refuses, such as one with a label that starts with
`__`, names itself by its labels, as it does in `ingest`. A text is written
whole, in one write, so it may hold more than one `ingest`: up to 100,000
samples, enough for kube-state-metrics or cadvisor on a busy cluster. While
a large text is written, other writes of metrics wait behind it. Bun, Node,
Python and Go send the text in one message, which holds 2 MiB.

## Limits

| What              | Limit                                                             |
|-------------------|-------------------------------------------------------------------|
| one `ingest`      | 10,000 samples or 4 MiB                                           |
| one text          | 100,000 samples or 32 MiB, and 2 MiB from Bun, Node, Python or Go |
| a series          | 128 labels with its name, 16 KiB; a label's value 4 KiB           |
| a sample's time   | from `keep` ago to ten minutes ahead                              |
| series in a store | 100,000 unless `maxSeries` says                                   |

A store given a memory budget reserves part of it for a text before reading a
line: about half a KiB a series beside its labels' bytes, so a text of
100,000 short series holds about 60 MiB of the budget while it is written.

## How many series

A store holds 100,000 series unless it is opened with more. A new series past
them is a `limit` error, and a series kept no longer gives its place back. Fifty
hosts of 2,000 series each fill the default, so a store that keeps what
exporters send asks for more when it opens:

```ts
const store = await open('./data', { maxSeries: 1_000_000 })
```

```python
store = await tinystore.open("./data", max_series=1_000_000)
```

```go
store, err := tinystore.Open(ctx, "./data", tinystore.MaxSeries(1_000_000))
```

```rust
let store = Store::open("./data", Options { max_series: 1_000_000, ..Options::default() })?;
```

```sh
tinystore serve ./data --max-series 1000000
```

A series costs `metrics.db` about 283 bytes before its samples pile up: 56 its
labels' postings, 53 its state, 51 its row, 46 the index of its identity, 40
its first head, 35 the indexes by name and by work due. The store keeps
nothing of it in memory. A million series of four labels, one sample each,
took 283 MB of pages and 126 seconds to write 2,000 an ingest, where 100,000
took 28 MB and 5.6 seconds: each new series costs more as the indexes grow.
Measured on an AMD Ryzen 7 7700 under Windows 11, Rust 1.99, `--release`, with
`TINYSTORE_SERIES=1000000 cargo test --release -p tinystore --lib --
metrics::ingest::tests::what_a_series_costs_the_file --ignored --nocapture`.

## See also

- [Metrics](README.md): the engine's overview.
- [Reading](reading.md): the samples back, bit for bit.
