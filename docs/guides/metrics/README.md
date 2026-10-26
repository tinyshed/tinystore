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
const p95 = await store.metrics.aggregate({ name: 'http_request_ms', since: '1h', every: '5m', op: 'p95' })
```

```python
requests = store.metrics.counter("http_requests_total")
latency = store.metrics.timer("http_request_ms", buckets=[5, 10, 25, 50, 100, 250, 500, 1000])

requests.labels(route="/notes").inc()
async with latency.measure():
    await handle(request)
p95 = await store.metrics.aggregate(name="http_request_ms", since="1h", every="5m", op="p95")
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

- A sample is kept bit for bit: `-0`, NaN payloads and infinities included.
- An aggregate is exact and rounded once, whatever blocks the samples were
  packed into; buckets of any width add up to the range.
- A query is bounded before it starts, and fails `limit` rather than answer
  less.
- Processes that count into one counter count into one total, and a send
  repeated after a lost connection counts once.
- An ingest sent again changes nothing, even after the store packed its
  samples (MET-16).
- Not promised: a sample with a new value behind its series' sealed frontier
  is refused (NOT-08).

## In these guides

| Page                                       | Covers                                                     |
|--------------------------------------------|------------------------------------------------------------|
| [Instruments](instruments.md)              | counters, gauges, gauge functions, timers, histograms      |
| [Reading](reading.md)                      | `read`, `latest`, names, labels                            |
| [Aggregates](aggregates.md)                | `op`, `every`, `by`, percentiles, gaps and partial buckets |
| [Ingesting](ingesting.md)                  | samples from elsewhere, `keep`, Prometheus exporters' text |
| [The store's own metrics](self-metrics.md) | `selfMetrics` and the `tinystore_` series                  |
| [The metrics crate](metrics-crate.md)      | Rust's `metrics` macros recorded into the store            |

New to Metrics? [How fast it is](../../handbook/chat/how-fast-it-is.md) in
the Handbook times a chat's model calls, and [How logs and metrics
pack](../../handbook/inside/how-logs-and-metrics-pack.md) explains the
format.

## Async Rust

In Rust, the `async` feature adds `tinystore::aio::metrics`, whose reads
and writes you await. This section shows only Rust, because Bun, Node and
Python already await every call, and in Go a call blocks its goroutine.

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

None of these calls commits, syncs the disk or waits on your executor's
thread (contract STORE-10). A write runs on the store's own threads and
waits there for its commit. Once its future is polled, the write commits
even if you drop the future (contract DUR-11). When an ingest has to wait
for the store's memory, it waits without holding one of the store's
threads, so ingests that wait for memory don't hold up your reads. A read
runs on the store's threads too. When you drop its future, it stops where
it is, even while it waits for a reader or for memory (contract STORE-09):

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
