# Aggregates

An aggregate splits a time range into buckets and computes one value for each
bucket: a count, a sum, a minimum, a maximum, an average, an increase, a rate
or a change. Every value is computed exactly and rounded once at the end, and
counter resets are handled for you.

## Requests per hour

```ts
const traffic = await store.metrics.aggregate({
	name: 'http_requests_total',
	since: '24h',
	width: '1h',
	op: 'increase',
})

for (const series of traffic) {
	for (const bucket of series.buckets) {
		console.log(bucket.from, bucket.value) // the start of the hour, and its requests
	}
}
```

```python
traffic = await store.metrics.aggregate(name="http_requests_total", since="24h", width="1h", op="increase")

for series in traffic:
    for bucket in series.buckets:
        print(bucket.from_, bucket.value)  # the start of the hour, and its requests
```

```go
traffic, err := stats.Aggregate(ctx, metrics.AggregateRequest{
	Range: metrics.Range{Name: "http_requests_total", Since: 24 * time.Hour},
	Width: time.Hour,
	Op:    metrics.AggregateIncrease,
})
for _, series := range traffic {
	for _, bucket := range series.Buckets {
		fmt.Println(bucket.From, bucket.Value) // the start of the hour, and its requests
	}
}
```

The buckets start at the beginning of the range and are `width` long. Only
buckets that contain samples are returned. Each bucket also says how many
samples it counted and how many counter resets it saw.

## Operations

| `op` | For | Value of a bucket |
|---|---|---|
| `count` | any | the number of samples |
| `sum` | gauges | the exact sum of the samples |
| `min`, `max` | gauges | the smallest or largest sample |
| `avg` | gauges | the exact sum divided by the count |
| `increase` | counters | how much the counter grew, resets included |
| `rate` | counters | the increase per second of the bucket |
| `delta` | gauges | the last sample minus the first |

In Go, the operations are `metrics.AggregateCount`, `AggregateSum`,
`AggregateMin`, `AggregateMax`, `AggregateAvg`, `AggregateIncrease`,
`AggregateRate` and `AggregateDelta`.

## Counter resets

A counter starts from zero when its process restarts. Within a bucket, a
sample lower than the one before it counts as a reset:

```text
samples   100 → 110 → 5 → 20
increase  10 + 5 + 15 = 30, with one reset
```

A summary of only the first and last values would say 20 − 100. TinyStore
computes the increase from every sample, and for blocks that were already
compressed, it stored the exact increase when it packed them.

An increase covers the samples inside its bucket. It doesn't add the step from
the last sample of one bucket to the first sample of the next.

## Group series

```ts
const byRoute = await store.metrics.aggregate({
	name: 'http_requests_total',
	since: '1h',
	width: '5m',
	op: 'rate',
	by: ['route'],
})
```

```python
by_route = await store.metrics.aggregate(
    name="http_requests_total", since="1h", width="5m", op="rate", by=["route"]
)
```

```go
byRoute, err := stats.Aggregate(ctx, metrics.AggregateRequest{
	Range: metrics.Range{Name: "http_requests_total", Since: time.Hour},
	Width: 5 * time.Minute,
	Op:    metrics.AggregateRate,
	By:    []string{"route"},
})
```

`by` joins the series that have the same values of the given labels: here,
one result per route, whatever the status or host. `without` keeps all labels
except the given ones, and `by: []` joins every series of the name into one.

Each series is computed exactly first, and then the series are joined, and the
result is rounded only once. A group's `avg` is the average of all samples of
its series, not the average of the series' averages.

## Exact arithmetic

- A sum is the exact sum of the samples, rounded once to the nearest float64.
  It doesn't depend on how the samples were stored or added.
- A sum that is too large for a float64 returns infinity, with an `overflow`
  flag on its bucket.
- A gauge bucket that contains `NaN` or an infinity fails with an error
  instead of returning a number.
- A counter sample below zero, `NaN` or infinite fails the query too.

## Retention cuts

Samples older than the retention are not counted. If the retention cutoff
falls inside a bucket, the bucket counts only the samples after it and is
marked as `partial`. A rate of a partial bucket is still divided by the
bucket's full width.

## See also

- [Reading](reading.md): conditions that select series, and query limits.
- [design/aggregate-contract.md](https://github.com/tinyshed/research/blob/main/tinystore/design/aggregate-contract.md)
  in the research repository: the arithmetic in full detail.
