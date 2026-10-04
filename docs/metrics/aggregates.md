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

| `op`         | For      | Value of a bucket                          |
|--------------|----------|--------------------------------------------|
| `count`      | any      | the number of samples                      |
| `sum`        | gauges   | the exact sum of the samples               |
| `min`, `max` | gauges   | the smallest or largest sample             |
| `avg`        | gauges   | the exact sum divided by the count         |
| `increase`   | counters | how much the counter grew, resets included |
| `rate`       | counters | the increase per second of the bucket      |
| `delta`      | gauges   | the last sample minus the one before it    |

In Go, the operations are `metrics.AggregateCount`, `AggregateSum`,
`AggregateMin`, `AggregateMax`, `AggregateAvg`, `AggregateIncrease`,
`AggregateRate` and `AggregateDelta`.

## Counter resets

A counter starts from zero when its process restarts. A sample lower than the
one before it counts as a reset:

```text
samples   100 → 110 → 5 → 20
increase  10 + 5 + 15 = 30, with one reset
```

A summary of only the first and last values would say 20 − 100. TinyStore
computes the increase from every sample, and for blocks that were already
compressed, it stored the exact increase when it packed them.

## Buckets add up to the range

An increase, a rate and a delta count each step between two samples in the
bucket where the step ends. So the buckets of a range add up to the whole
range, whatever their width:

```text
a counter that grows by 1 per second, sampled every 15 s for one hour
1h buckets   3585
5m buckets   285 + 300 + 300 + … + 300 = 3585
1m buckets    45 +  60 +  60 + … +  60 = 3585
```

The first bucket counts the step from the last sample before the range, if
that sample is at most one bucket width older. Such a bucket has the
`lookback` flag. If there is no sample that close, the first bucket counts
only the steps between its own samples, as the first bucket of each width
above does. To look further back, set `lookback` (`Lookback` in Go):

```ts
const perMinute = await store.metrics.aggregate({
	name: 'http_requests_total',
	since: '1h',
	width: '1m',
	op: 'increase',
	lookback: '10m', // the first step may start up to 10 minutes before the range
})
```

```python
per_minute = await store.metrics.aggregate(
    name="http_requests_total", since="1h", width="1m", op="increase", lookback="10m"
)
```

```go
perMinute, err := stats.Aggregate(ctx, metrics.AggregateRequest{
	Range:    metrics.Range{Name: "http_requests_total", Since: time.Hour},
	Width:    time.Minute,
	Op:       metrics.AggregateIncrease,
	Lookback: 10 * time.Minute, // the first step may start up to 10 minutes before the range
})
```

The other operations don't use `lookback`. A count, a sum, a minimum, a
maximum and an average use only the samples inside each bucket.

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

The same applies to the step into the first bucket. If the last sample before
the range has already expired, its step isn't counted, and the first bucket is
marked as `partial`.

## See also

- [Reading](reading.md): conditions that select series, and query limits.
- [design/aggregate-contract.md](https://github.com/tinyshed/research/blob/main/tinystore/design/aggregate-contract.md)
  in the research repository: the arithmetic in full detail. It was written
  before buckets counted the step into them.
