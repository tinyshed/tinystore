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
let p95 = stats.query().name("http_request_ms").since(Duration::from_hours(1)).every(Duration::from_mins(5)).aggregate(Op::Percentile(95.0))?;
```

## What it guarantees

- A sample is kept bit for bit: `-0`, NaN payloads and infinities included.
- An aggregate is exact and rounded once, whatever blocks the samples were
  packed into; buckets of any width add up to the range.
- A query is bounded before it starts, and fails `limit` rather than answer
  less.
- Processes that count into one counter count into one total, and a send
  repeated after a lost connection counts once.
- Not promised: a sample behind its series' sealed frontier is refused
  (NOT-08).

## In these guides

| Page                    | Covers                                                     |
|-------------------------|------------------------------------------------------------|
| Instruments             | counters, gauges, gauge functions, timers, histograms      |
| Reading                 | `read`, `latest`, names, labels                            |
| Aggregates              | `op`, `every`, `by`, percentiles, gaps and partial buckets |
| Ingesting               | samples from elsewhere, `keep`                             |
| Prometheus              | Prometheus' text in                                        |
| The store's own metrics | `selfMetrics` and the `tinystore_` series                  |
| The metrics crate       | Rust's `metrics` macros recorded into the store            |

New to Metrics? [How fast it is](../../handbook/chat/how-fast-it-is.md) in
the Handbook times a chat's model calls, and [How logs and metrics
pack](../../handbook/inside/how-logs-and-metrics-pack.md) explains the
format.

## Reference

[Bun and Node](../../reference/bun/metrics.md) ·
[Python](../../reference/python/metrics.md) ·
[Go](../../reference/go/metrics.md) ·
[Rust](../../reference/rust/metrics.md)
