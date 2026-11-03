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
  once. A crash of the program loses none. A power loss may take the last
  ones, as [the durability of
  `metrics.db`](../../handbook/start/directory.md#a-file-per-engine) says.
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
  fails the whole call. A packed counter's increase would change otherwise
  (NOT-08). The same sample sent again changes nothing, as [the next
  section](#an-ingest-sent-again) shows. To change a gauge's packed samples,
  use [`replace`](#correcting-a-gauge-after-the-fact).
- A series that comes back after a day away sends its backlog: what it has
  packed follows its own newest sample, not the clock.
- A time is taken from as long ago as its name keeps to ten minutes ahead of
  the store's clock. Outside that, the call fails with a too old or too new
  error, even for a sample sent again. [A window for every
  time](../../handbook/inside/time-and-errors.md#a-window-for-every-time)
  says why.

## An ingest sent again

A [job](../jobs/README.md#what-it-guarantees) runs at least once, so a
collector's retry may send the same samples again. That second call changes
nothing, even after the store has packed those samples:

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
- `ingest` can't change a value after the fact, such as yesterday's revenue
  after a refund. For a gauge, `replace` can, as the next section shows.

## Correcting a gauge after the fact

Some numbers change after you write them. A refund lowers yesterday's
revenue, and a late order raises it. A collector that reads the last few
days again can't `ingest` the new values once the store has packed them.
`replace` swaps every sample of a range for the samples you give:

```ts
const day = (back: number) => new Date(Date.UTC(2026, 10, 3 - back)) // midnight, `back` days before 3 November
// the last three days, read again from the shop's database: a refund took 100 off yesterday
const revenue = { name: 'revenue', kind: 'gauge', labels: { shop: 'msk' }, samples: [[day(3), 1000], [day(2), 1000], [day(1), 900]] } as const
await store.metrics.replace(revenue, { from: day(3), to: day(0) })
await store.metrics.replace(revenue, { from: day(3), to: day(0) }) // the next run reads the same: nothing changes
```

```python
def day(back: int) -> datetime:  # midnight, `back` days before 3 November
    return datetime(2026, 11, 3, tzinfo=UTC) - timedelta(days=back)

# the last three days, read again from the shop's database: a refund took 100 off yesterday
samples = [(day(3), 1000), (day(2), 1000), (day(1), 900)]
revenue = Series(name="revenue", kind="gauge", labels={"shop": "msk"}, samples=samples)
await store.metrics.replace(revenue, from_=day(3), to=day(0))
await store.metrics.replace(revenue, from_=day(3), to=day(0))  # the next run reads the same: nothing changes
```

```go
day := func(back int) time.Time { return time.Date(2026, 11, 3-back, 0, 0, 0, 0, time.UTC) } // midnight, back days before 3 November
// the last three days, read again from the shop's database: a refund took 100 off yesterday
revenue := metrics.Series{Name: "revenue", Kind: metrics.KindGauge, Labels: metrics.Labels{"shop": "msk"},
	Samples: []metrics.Sample{{At: day(3), Value: 1000}, {At: day(2), Value: 1000}, {At: day(1), Value: 900}}}
err := metrics.Replace(ctx, store, revenue, day(3), day(0))
err = metrics.Replace(ctx, store, revenue, day(3), day(0)) // the next run reads the same: nothing changes
```

```rust
let stats = store.metrics()?;
let day = |back: u64| UNIX_EPOCH + Duration::from_secs(1_793_664_000 - back * 86_400); // midnight, `back` days before 3 November
// the last three days, read again from the shop's database: a refund took 100 off yesterday
let revenue = Series::gauge("revenue").label("shop", "msk");
let revenue = revenue.sample(day(3), 1000.0).sample(day(2), 1000.0).sample(day(1), 900.0);
stats.replace(&revenue, day(3)..day(0))?;
stats.replace(&revenue, day(3)..day(0))?; // the next run reads the same: nothing changes
```

- The range starts at `from` and ends before `to`. After the call, it holds
  exactly the samples you gave. A time you give takes its value, a time that
  wasn't there is added, and a time you leave out is deleted. A sample
  outside the range fails with an invalid error.
- `replace` is one write. A read or an aggregate sees the range as it was or
  as it is now, never half of each. An aggregate over the range answers from
  the new samples, exactly as a read of them adds up.
- The same samples sent again change nothing and write nothing, so a
  collector can read the last few days again on every run.
- Only a gauge can be replaced. A counter's increase is added up across what
  the store packed, so a counter or a histogram fails with an invalid error.
- A series that doesn't exist yet is written. A series that a replace leaves
  without samples is deleted, as if its samples had expired.
- The range must start inside what the name keeps and end at most ten minutes
  ahead of the store's clock. Otherwise the call fails with a too old or too
  new error.
- One call takes as many samples as one `ingest`, 10,000. What the range
  already holds is bounded as a [query](reading.md) is. Past either bound,
  the call fails with a limit error, so replace a long range in several
  calls.

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
`__`, names itself by its labels, as in `ingest`. A text is written whole, in
one write, so it may hold more samples than one `ingest`: enough for
kube-state-metrics or cadvisor on a busy cluster. While a large text is
written, other writes of metrics wait behind it.

Bun, Node, Python and Go send a text over 2 MiB in pieces. The store holds the
pieces in its `memory` budget and writes nothing until the last one arrives,
so a call cancelled or cut off before then writes nothing. A text over 32 MiB
fails with a limit error (`LimitError`, `ErrLimit`) before the store keeps any
of it.

## Limits

| What              | Limit                                                                      |
|-------------------|----------------------------------------------------------------------------|
| one `ingest`      | 10,000 samples or 4 MiB                                                    |
| one `replace`     | 10,000 samples or 4 MiB, and its range read within a query's bounds        |
| one text          | 100,000 samples or 32 MiB, and 32 MiB of text from Bun, Node, Python or Go |
| a series          | 128 labels with its name, 16 KiB; a label's value 4 KiB                    |
| a sample's time   | from `keep` ago to ten minutes ahead                                       |
| series in a store | 100,000 unless `maxSeries` says                                            |

A store given a memory budget reserves part of it for a text before reading a
line: about half a KiB a series beside its labels' bytes, so a text of
100,000 short series holds about 60 MiB of the budget while it is written,
beside the text's own bytes when it came in pieces.

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

Before its samples pile up, a series costs `metrics.db` the postings of its
labels, its state, its row, the index of its identity, its first head, and
its place in the indexes by name and by work due. The store keeps none of it
in memory. Each new series takes longer to write than the one before, as the
indexes grow. To measure it on your machine, this test writes as many series
as `TINYSTORE_SERIES` says and prints what each b-tree of the file costs per
series:

```sh
TINYSTORE_SERIES=1000000 cargo test --release -p tinystore --lib -- metrics::ingest::tests::what_a_series_costs_the_file --ignored --nocapture
```

## See also

- [Metrics](README.md): the engine's overview.
- [Reading](reading.md): the samples back, bit for bit.
