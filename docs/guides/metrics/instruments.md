# Instruments

A program measures itself with instruments: counters, gauges, timers and
histograms. A call changes a number in memory and never waits; every 15
seconds the instruments send what changed to the store, which adds it to the
totals it keeps and writes them. Every process counting into one counter
counts into one total.

```ts
const requests = store.metrics.counter<'route' | 'status'>('http_requests_total', { help: 'Requests served.' })
requests.with({ route: '/notes', status: 200 }).inc()

const depth = store.metrics.gauge('queue_depth', { unit: 'jobs' })
depth.set(12)
store.metrics.gaugeFunc('open_tickets', () => db.scalar`select count(*) from tickets where open`)

const latency = store.metrics.timer('http_request_ms', { buckets: [5, 10, 25, 50, 100, 250, 500, 1000] })
const user = await latency.with({ route: '/users' }).measure(() => users.get(id))
latency.with({ route: '/sync' }).record(performance.now() - started) // milliseconds
```

```python
requests = store.metrics.counter("http_requests_total", help="Requests served.")
requests.labels(route="/notes", status=200).inc()

depth = store.metrics.gauge("queue_depth", unit="jobs")
depth.set(12)
store.metrics.gauge_func("open_tickets", lambda: db.scalar("select count(*) from tickets where open"))

latency = store.metrics.timer("http_request_ms", buckets=[5, 10, 25, 50, 100, 250, 500, 1000])
async with latency.labels(route="/users").measure():
    user = await users.get(user_id)
latency.labels(route="/sync").record(elapsed)  # a timedelta
```

```go
requests, err := metrics.OpenCounter(store, "http_requests_total", metrics.Help("Requests served."))
requests.With("route", "/notes", "status", "200").Inc()

depth, err := metrics.OpenGauge(store, "queue_depth", metrics.Unit("jobs"))
depth.Set(12)
err = metrics.OpenGaugeFunc(store, "open_tickets", func(ctx context.Context) (float64, error) {
	return sqldb.Scalar[float64](ctx, db, `select count(*) from tickets where open`)
})

latency, err := metrics.OpenTimer(store, "http_request_ms", metrics.Buckets(5, 10, 25, 50, 100, 250, 500, 1000))
defer latency.With("route", "/users").Start().Stop() // records when the function returns
latency.With("route", "/sync").Record(time.Since(started))
```

```rust
let stats = store.metrics()?;
let requests = stats.counter("http_requests_total").help("Requests served.").open()?;
requests.with(&[("route", "/notes"), ("status", "200")]).inc();

let depth = stats.gauge("queue_depth").unit("jobs").open()?;
depth.set(12.0);

let latency = stats.timer("http_request_ms").buckets(&[5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0]).open()?;
let user = latency.with(&[("route", "/users")]).measure(|| users.get(id));
```

## The four kinds

| Instrument | Calls                                  | Kept as                                                          |
|------------|----------------------------------------|------------------------------------------------------------------|
| counter    | `inc()`, `add(n)`                      | a total that only grows, the store's, every process adding to it |
| gauge      | `set`, `inc`, `dec`, `add`             | its value at each write, every 15 seconds                        |
| timer      | `measure(fn)`, `record(ms)`, `start()` | a count, a sum in ms and the longest; with `buckets`, a spread   |
| histogram  | `record(n)`                            | a timer of numbers that are not durations: sizes, scores         |

- An instrument is made once and kept. `with` (`labels` in Python, `With` in
  Go) gives the same instrument with more labels, each set of labels a
  series of its own. A label's value is text: `200` is kept as `"200"`.
- A counter's negative `add` throws `invalid` at once, and so does a NaN or an
  infinity given to any instrument, where the program made it.
- `add(0)` makes a set of labels known with a total of zero, so a chart shows
  0 before the first count.
- In TypeScript, `counter<'route' | 'status'>(…)` takes only those labels in
  `with`, checked as they are written.

## One total across processes

```text
4 workers, each counting http_requests_total{route="/notes"}
the store's series: every request counted once
```

The store keeps a counter's total, and a timer's count, sum and buckets, so
processes that share a store, through a sidecar or a server, count into one
series. A send whose connection was lost goes again on the next and counts
once. A crash loses at most the last 15 seconds.

A gauge is the state of the process that sets it, so two processes setting
one gauge overwrite each other: give each a label of its own, `instance`.

Each run of a total starts with a zero at the moment it began, so a restart is
seen as a reset: `2` at 11:00, `0` at 12:00 and `5` at 12:15 are an increase
of 5, where `2` and `5` alone would say 3.

## Timers

A timer named `http_request_ms` writes these series:

| Series                   | Kind    | Holds                                      |
|--------------------------|---------|--------------------------------------------|
| `http_request_ms_count`  | counter | how many durations                         |
| `http_request_ms_sum`    | counter | their sum in milliseconds                  |
| `http_request_ms_max`    | gauge   | the longest since the write before         |
| `http_request_ms_bucket` | counter | with `buckets`, one a bound, labelled `le` |

A query of `http_request_ms` reads them together: `count`, `avg`, `max` and,
with buckets, percentiles. A timer without `buckets` has no percentile.

- `measure` runs a function, awaits what it returns, and records how long it
  took whether it returned or threw.
- `start()` gives a timing that records when it stops, for a span that is no
  one function. In Rust it records when it drops: keep it in a name,
  `let _timing = latency.start()`.

## Buckets

```ts
store.metrics.timer('http_request_ms', { buckets: [5, 10, 25, 50, 100, 250, 500, 1000] })
store.metrics.timer('job_ms', { buckets: exponentialBuckets(1, 2, 14) }) // 1, 2, 4 … 8192
store.metrics.timer('db_query_ms', { buckets: 'exponential' }) // the store chooses the bounds
```

```python
store.metrics.timer("http_request_ms", buckets=[5, 10, 25, 50, 100, 250, 500, 1000])
store.metrics.timer("job_ms", buckets=exponential_buckets(1, 2, 14))
store.metrics.timer("db_query_ms", buckets="exponential")
```

```go
latency, err := metrics.OpenTimer(store, "http_request_ms", metrics.Buckets(5, 10, 25, 50, 100, 250, 500, 1000))
jobs, err := metrics.OpenTimer(store, "job_ms", metrics.Buckets(metrics.ExponentialBuckets(1, 2, 14)...))
queries, err := metrics.OpenTimer(store, "db_query_ms", metrics.Exponential())
```

```rust
let api = stats.timer("http_request_ms").buckets(&[5.0, 10.0, 25.0, 50.0, 100.0]).open()?;
let jobs = stats.timer("job_ms").buckets(exponential_buckets(1.0, 2.0, 14)).open()?;
let db = stats.timer("db_query_ms").buckets(Buckets::Exponential).open()?;
```

- The store adds `+Inf`, so a duration past the last bound is counted above
  it rather than lost.
- `'exponential'`, an experiment before Prometheus' native histograms, keeps
  each value in an interval of their grid, eight a doubling, about 9 % wide; a
  percentile then lies within one interval, with no bound to choose.
- A name's buckets are its own: another set while the store is open is
  `invalid`. A percentile across a change uses only the bounds both sets
  share, and its interval widens to say so.

## A gauge read from a function

`gaugeFunc` calls its function at each write, for a value the program reads
from elsewhere: a queue's depth, a pool's size. It may be async. One that has
not answered in time writes nothing that time and holds up no other
instrument; an error writes nothing, never a zero. In Rust it runs on the
store's thread and must not block.

```ts
store.metrics.gaugeFunc('queue_depth', () => emails.depth(), { labels: { queue: 'emails' } })
store.metrics.gaugeFunc('queue_depth', () => sms.depth(), { labels: { queue: 'sms' } })
```

```python
store.metrics.gauge_func("queue_depth", emails.depth, labels={"queue": "emails"})
store.metrics.gauge_func("queue_depth", sms.depth, labels={"queue": "sms"})
```

```go
err = metrics.OpenGaugeFunc(store, "queue_depth", emails.Depth, metrics.Labels{"queue": "emails"})
err = metrics.OpenGaugeFunc(store, "queue_depth", sms.Depth, metrics.Labels{"queue": "sms"})
```

```rust
stats.gauge_func("queue_depth", move || emails.depth()).labels(&[("queue", "emails")]).open()?;
stats.gauge_func("queue_depth", move || sms.depth()).labels(&[("queue", "sms")]).open()?;
```

## How many series

- An instrument takes at most 2,000 sets of labels between two sends. A new
  set past them counts into the series `{ __overflow: 'true' }`, said once, so
  a label holding a user's id costs no memory past the bound and the total
  stays whole.
- A counter's or a timer's series that nothing sent for an hour leaves the
  store's memory; sent again, it starts a new run with its zero.
- A store holds at most 100,000 series.

Keep labels to what a chart groups by: a route, a status, a queue. A user's
id, a request's id or a URL with ids in it belongs in [records](../records/README.md).

## Flush and tests

```ts
await store.metrics.flush() // written now; before a short program exits
```

```python
await store.metrics.flush()
```

```go
err = metrics.Flush(ctx, store)
```

```rust
stats.flush()?;
```

A store opened with `background: false` writes instruments only at `flush()`
and when it closes, so a test flushes before it reads.

## Options

| Option    | Unless given | Does                                                        |
|-----------|--------------|-------------------------------------------------------------|
| `unit`    | none         | the unit of its values: `'bytes'`, `'%'`; a timer's is `ms` |
| `help`    | none         | a line on what it means                                     |
| `keep`    | the name's   | how long the name's samples stay, 30 days unless set        |
| `buckets` | none         | a timer's or a histogram's upper bounds, ascending          |

## See also

- [Metrics](README.md): the engine's overview.
- [Aggregates](aggregates.md): reading instruments back as rates and
  percentiles.
- [How fast it is](../../handbook/chat/how-fast-it-is.md) in the Handbook.
