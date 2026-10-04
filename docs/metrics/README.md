# Metrics

The metrics engine stores numbers over time: requests per route, queue depth,
response times. Your program counts with counters, gauges and timers, and the
engine stores their values every 15 seconds. Every sample is kept bit for bit,
and every aggregate is computed exactly and rounded only once. Metrics keeps
its data in `data/metrics.db`.

## Count and measure

```ts
const requests = store.metrics.counter('http_requests_total')
const latency = store.metrics.timer('http_request_ms')

const user = await latency.with({ route: '/users' }).measure(() => users.get(id))
requests.with({ route: '/users', status: '200' }).inc()
```

```python
requests = store.metrics.counter("http_requests_total")
latency = store.metrics.timer("http_request_ms")

with latency.labels(route="/users").measure():
    user = await users.get(user_id)
requests.labels(route="/users", status="200").inc()
```

```go
stats, err := metrics.Open(ctx, store, metrics.Options{}) // data/metrics.db
requests := stats.Counter("http_requests_total")
latency := stats.Timer("http_request_ms")

start := time.Now()
user, err := users.Get(ctx, id)
latency.With("route", "/users").Since(start)
requests.With("route", "/users", "status", "200").Inc()
```

Create an instrument once and keep it. Its labels pick a series: each
combination of labels, such as a route and a status, is a series of its own.

## Ask how much

```ts
const traffic = await store.metrics.aggregate({
	name: 'http_requests_total',
	since: '24h',
	width: '1h',
	op: 'increase',
	by: ['route'],
})
```

```python
traffic = await store.metrics.aggregate(
    name="http_requests_total", since="24h", width="1h", op="increase", by=["route"]
)
```

```go
traffic, err := stats.Aggregate(ctx, metrics.AggregateRequest{
	Range: metrics.Range{Name: "http_requests_total", Since: 24 * time.Hour},
	Width: time.Hour,
	Op:    metrics.AggregateIncrease,
	By:    []string{"route"},
})
```

This returns, for each route, how many requests it served in each hour of the
last day. A process that restarted reset its counters to zero, and `increase`
counts those resets correctly.

## Exact, not approximate

Most metrics systems round, interpolate and extrapolate. TinyStore doesn't:

- A sample comes back with the same bits it was stored with, including `-0`
  and the payload of a `NaN`.
- A sum is computed exactly and rounded once, so its result doesn't depend on
  how the samples were stored or in which order they were added.
- A bucket contains only the samples in its time range, with no
  interpolation to its edges.
- When a query hits a limit, it fails with a limit error instead of returning a
  smaller answer.

There are no histograms, because a percentile from buckets is an
approximation. A timer stores the count, the sum and the longest duration
instead, which give an exact mean and maximum.

## In this section

- [Instruments](instruments.md): counters, gauges, timers and gauge functions.
- [Reading](reading.md): read the raw samples of series.
- [Aggregates](aggregates.md): sums, rates, averages and groups of series.
- [Ingesting](ingesting.md): store samples that come from elsewhere.

## Limits and defaults

|                                |                                             |
|--------------------------------|---------------------------------------------|
| Retention                      | 30 days                                     |
| Instruments write their values | every 15 seconds, and when the store closes |
| Series                         | 100,000                                     |
| Labels per series              | 128 pairs                                   |
| Samples a query returns        | 100,000                                     |
| A query's snapshot             | 5 seconds                                   |

Store options set these limits, and a query can only lower them for itself.

## See also

- [metrics/README.md](../../metrics/README.md): the full contract of the
  metrics engine.
- [design/metrics.md](https://github.com/tinyshed/research/blob/main/tinystore/design/metrics.md)
  in the research repository: the storage design and its measurements.
