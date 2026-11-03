# Metrics

The Metrics engine keeps time series in `metrics.db`: counters, gauges,
timers and histograms a program measures itself with, and samples ingested
from elsewhere. Every sample is kept bit for bit, and an aggregate answers
each bucket exactly, rounded once.

```ts
const requests = store.metrics.counter<'route'>('http_requests_total')
const latency = store.metrics.timer('http_request_ms', { buckets: [5, 10, 25, 50, 100, 250, 500, 1000] })

requests.with({ route: '/notes' }).inc()
await latency.measure(() => handle(request))
const { groups: p95 } = await store.metrics.aggregate({ name: 'http_request_ms', since: '1h', every: '5m', op: 'p95' })
```

```python
requests = store.metrics.counter("http_requests_total")
latency = store.metrics.timer("http_request_ms", buckets=[5, 10, 25, 50, 100, 250, 500, 1000])

requests.labels(route="/notes").inc()
async with latency.measure():
    await handle(request)
p95 = (await store.metrics.aggregate(name="http_request_ms", since="1h", every="5m", op="p95")).groups
```

```go
requests, err := metrics.OpenCounter(store, "http_requests_total")
latency, err := metrics.OpenTimer(store, "http_request_ms", metrics.Buckets(5, 10, 25, 50, 100, 250, 500, 1000))

requests.With("route", "/notes").Inc()
timing := latency.Start()
handle(request)
timing.Stop()
p95, err := metrics.Aggregate(ctx, store, metrics.Query{
	Name: "http_request_ms", Since: time.Hour, Every: 5 * time.Minute, Op: metrics.Percentile(95),
})
```

```rust
let stats = store.metrics()?;
let requests = stats.counter("http_requests_total").open()?;
let latency = stats
    .timer("http_request_ms")
    .buckets(Buckets::Bounds(vec![5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0]))
    .open()?;

requests.with(&[("route", "/notes")]).inc();
latency.measure(|| handle(&request));
let p95 = stats.query().name("http_request_ms").since(Duration::from_hours(1)).every(Duration::from_mins(5)).aggregate(Op::Percentile(95.0))?.groups;
```

## What it guarantees

- A sample is kept bit for bit: `-0`, NaN payloads and infinities included
  (MET-01).
- An aggregate is exact whatever blocks the samples were packed into, and
  [buckets](aggregates.md#buckets-add-up-to-the-range) of any width add up to
  the range (MET-04).
- A query is bounded before it starts, and fails `limit` rather than answer
  less. Given `limit`, it answers [in pages](reading.md#read-a-long-range-in-pages)
  that give exactly the whole answer while the data doesn't change (MET-17).
- Processes that count into one counter count into one total, and a send
  repeated after a lost connection counts once (MET-09).
- An ingest sent again changes nothing, even after the store packed its
  samples (MET-16).
- A gauge's past changes only through
  [`replace`](ingesting.md#correcting-a-gauge-after-the-fact), in one write
  that a read sees whole or not at all (MET-19).
- An aggregate of [series that you hold in
  memory](aggregates.md#samples-you-dont-store), in Rust, gives what a query
  of the same samples in a store gives (MET-18).
- Not promised: `ingest` refuses a sample with a new value behind what its
  series has packed, and a counter's packed samples never change (NOT-08).

## In these guides

| Page                                       | Covers                                                     |
|--------------------------------------------|------------------------------------------------------------|
| [Instruments](instruments.md)              | counters, gauges, gauge functions, timers, histograms      |
| [Reading](reading.md)                      | `read`, pages of a long range, `latest`, names, labels     |
| [Aggregates](aggregates.md)                | `op`, `every`, `by`, percentiles, gaps and partial buckets |
| [Ingesting](ingesting.md)                  | samples from elsewhere, `replace`, `keep`, exporters' text |
| [The store's own metrics](self-metrics.md) | `selfMetrics` and the `tinystore_` series                  |
| [The metrics crate](metrics-crate.md)      | Rust's `metrics` macros recorded into the store            |

New to Metrics? [How fast it is](../../handbook/chat/how-fast-it-is.md) in
the Handbook times a chat's model calls, and [How logs and metrics
pack](../../handbook/inside/how-logs-and-metrics-pack.md) explains the
format.

## Async Rust

In Rust, the `async` feature adds `tinystore::aio::metrics`, whose reads
and writes you await. [Rust with async](../../handbook/start/rust-async.md)
has what every engine shares: where a call runs, a dropped future, timeouts
and closing.

```rust
use std::time::{Duration, SystemTime};
use tinystore::Options;
use tinystore::aio::Store;
use tinystore::aio::metrics::{Op, Series};

let store = Store::open("data", Options::default())?;
let stats = store.metrics()?; // a plain call, as in blocking Rust
let requests = stats.counter("http_requests_total").open()?; // instruments are plain calls too
requests.with(&[("route", "/notes")]).inc();

stats.ingest(&[Series::gauge("cpu").label("host", "web-1").sample(SystemTime::now(), 0.42)]).await?;
let cpu = stats.query().name("cpu").since(Duration::from_hours(1)).read().await?.series;
let p95 = stats
    .query()
    .name("http_request_ms")
    .since(Duration::from_hours(1))
    .every(Duration::from_mins(5))
    .aggregate(Op::Percentile(95.0))
    .await?
    .groups;
```

Each call has the same name, builder and answer as in blocking Rust, and
you add `.await`:

| Blocking Rust                                            | Async Rust                                                      |
|----------------------------------------------------------|-----------------------------------------------------------------|
| `stats.ingest(&series)?`                                 | `stats.ingest(&series).await?`                                  |
| `stats.replace(&series, from..to)?`                      | `stats.replace(&series, from..to).await?`                       |
| `stats.prometheus().ingest(&body)?`                      | `stats.prometheus().ingest(&body).await?`                       |
| `query.read()?`, `aggregate`, `latest`, `explain`        | `query.read().await?`, `aggregate`, `latest`, `explain`         |
| `stats.names().page()?`, `label_names`, `label_values`   | `stats.names().page().await?`, `label_names`, `label_values`    |
| `stats.describe("cpu", description)?`                    | `stats.describe("cpu", description).await?`                     |
| `stats.description("cpu")?`                              | `stats.description("cpu").await?`                               |
| `stats.damaged()?`                                       | `stats.damaged().await?`                                        |
| `stats.delete("cpu", [("host", "web-2")])?`              | `stats.delete("cpu", [("host", "web-2")]).await?`               |
| `stats.flush()?`                                         | `stats.flush().await?`                                          |
| `requests.inc()`, `gauge.set(0.5)`, `timer.record(took)` | the same: an instrument counts in memory, so you don't await it |

The values are the blocking module's own types, so `tinystore::aio::metrics`
exports `Series`, `Op`, `Description` and the others under the same names.

When an ingest has to wait for the store's memory, it waits without holding
one of the store's threads, so ingests that wait for memory don't hold up
your reads. A read whose future you drop stops even while it waits for a
reader or for memory, so a timeout bounds it:

```rust
let month = stats.query().name("cpu").since(Duration::from_hours(30 * 24));
match tokio::time::timeout(Duration::from_secs(2), month.read()).await {
    Ok(series) => draw(series?),
    Err(_) => {} // the read stopped, and its reader and memory went back to the store
}
```

A query is checked when its future is first polled. That is when `since`
becomes an absolute start, and a query that is not valid fails at once,
without using a thread. The first poll is also when a Prometheus text's
`ingest` reads the store's clock for the samples that have no time of
their own. `description` reads one row: it answers in that first poll when
a reader is free, and on the store's threads otherwise.

A blocking handle and an async one cross both ways:
`tinystore::aio::metrics::Metrics::from(blocking)` makes an async handle of
the same metrics, and `stats.blocking()` gives the blocking one back.

## Reference

[Bun and Node](../../reference/bun/metrics.md) ·
[Python](../../reference/python/metrics.md) ·
[Go](../../reference/go/metrics.md) ·
[Rust](../../reference/rust/metrics.md)
