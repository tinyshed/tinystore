# Aggregates

`aggregate` cuts a range into buckets of `every` and answers each with one
exact value: an increase, a rate, an average, a percentile. It is what a
dashboard draws: requests per minute by route, the p95 latency of each five
minutes, the average queue depth of each hour.

```ts
const { groups: traffic } = await store.metrics.aggregate({
	name: 'http_requests_total',
	since: '24h',
	every: '1h',
	op: 'increase',
	by: ['route'],
})
// [{ name, labels: { route: '/notes' }, buckets: [{ at, value: 1840, samples: 240, resets: 0 }, …] }, …]

const { groups: p95 } = await store.metrics.aggregate({ name: 'http_request_ms', since: '1h', every: '5m', op: 'p95' })
// [{ name, labels: {}, buckets: [{ at, lower: 25, upper: 50, samples: 3812 }, …] }]
```

```python
traffic = (await store.metrics.aggregate(name="http_requests_total", since="24h", every="1h", op="increase", by=["route"])).groups
p95 = (await store.metrics.aggregate(name="http_request_ms", since="1h", every="5m", op="p95")).groups
```

```go
traffic, err := metrics.Aggregate(ctx, store, metrics.Query{Name: "http_requests_total", Since: 24 * time.Hour,
	Every: time.Hour, Op: metrics.Increase, By: []string{"route"}})
p95, err := metrics.Aggregate(ctx, store, metrics.Query{Name: "http_request_ms", Since: time.Hour, Every: 5 * time.Minute, Op: metrics.Percentile(95)})
```

```rust
let stats = store.metrics()?;
let traffic = stats
    .query()
    .name("http_requests_total")
    .since(Duration::from_hours(24))
    .every(Duration::from_hours(1))
    .by(&["route"])
    .aggregate(Op::Increase)?.groups;
let p95 = stats.query().name("http_request_ms").since(Duration::from_hours(1)).every(Duration::from_mins(5)).aggregate(Op::Percentile(95.0))?.groups;
```

## Ops

| `op`            | Of                        | A bucket's answer                                                              |
|-----------------|---------------------------|--------------------------------------------------------------------------------|
| `count`         | any                       | how many samples; a timer's, how many durations                                |
| `sum`, `avg`    | a gauge, a timer          | their exact sum; that over their count                                         |
| `min`, `max`    | a gauge; `max` of a timer | the least and greatest sample; a timer's longest                               |
| `increase`      | a counter                 | how much it grew, resets counted                                               |
| `rate`          | a counter, a timer        | that a second of the bucket                                                    |
| `delta`         | a gauge                   | its last sample less the one before its first                                  |
| `first`, `last` | a gauge, a counter        | the bucket's first and last sample, bit for bit                                |
| `p50`, `p95` …  | a gauge, a timer          | a gauge's sample at that rank; a timer's bucket holding it, `lower` to `upper` |

Every value is exact and rounded once: a sum is the exact sum of its samples,
rounded to the nearest float, whatever order or blocks they were kept in.

## Buckets add up to the range

Buckets are spans of `every` counted from 1970 in UTC, so `1h` buckets begin
on the hour. An increase, a rate and a delta count each step between two
samples in the bucket where the step ends, and the first bucket takes its step
from the newest sample before the range:

```text
a counter growing by 1 a second, sampled every 15 s, read 10:00 to 11:00
1h buckets   3600
5m buckets   300 + 300 + 300 + … + 300 = 3600
1m buckets    60 +  60 +  60 + … +  60 = 3600
```

Buckets of any width add up to the range, and two adjacent ranges to the
range they make. Nothing is interpolated or extrapolated to a bucket's edges,
unlike Prometheus' `increase`, whose answer changes with the window.

## Days in a time zone

A day of `every: '1d'` runs from midnight to midnight in UTC. A shop in
Moscow counts its orders from its own midnight, which is 21:00 in UTC. Ask
for buckets of the calendar instead, with the zone's IANA name:

```ts
const { groups: days } = await store.metrics.aggregate({ name: 'orders', since: '30d', op: 'sum', calendar: 'day', timeZone: 'Europe/Moscow' })
```

```python
days = (await store.metrics.aggregate(name="orders", since="30d", op="sum", calendar="day", time_zone="Europe/Moscow")).groups
```

```go
days, err := metrics.Aggregate(ctx, store, metrics.Query{Name: "orders", Since: 30 * 24 * time.Hour, Op: metrics.Sum,
	Calendar: tinystore.Day, TimeZone: "Europe/Moscow"})
```

```rust
let days = stats
    .query()
    .name("orders")
    .since(Duration::from_hours(30 * 24))
    .calendar(Calendar::Day, "Europe/Moscow")
    .aggregate(Op::Sum)?.groups;
```

- `calendar` is `day`, `week` or `month`. A week begins on Monday, as ISO 8601
  counts weeks, and a month on its first day, each at local midnight.
- A bucket's `at` is the instant its local day, week or month begins.
- A day is 23 or 25 hours long on the days the clock moves, and a month is as
  long as it is. A `rate` divides by the seconds its bucket has.
- `calendar` takes a time zone, and goes in place of `every`. Both at once
  fail with an invalid error, and so does a zone no one has.

## Partial buckets and gaps

| A bucket says   | When                                               | A chart                                           |
|-----------------|----------------------------------------------------|---------------------------------------------------|
| `partial: true` | the range, the store's clock or retention cut it   | shows it apart, or leaves the unfinished edge out |
| `gap: true`     | a whole bucket of `every` before it had no samples | shows a gap, or asks a wider `every`              |

```text
http_requests_total   100 on 1 October, nothing since, 300 at 11:59 on 11 October
11:00 to 12:00        { value: 200, gap: true }
```

What a counter grew over a gap is counted in the bucket after it, since when
it grew is not known. A bucket without samples is left out of the answer.

## Percentiles

- `p95` of a gauge is the sample at that rank, nearest rank: a value that was
  stored, exact.
- `p95` of a timer is the bucket of its spread that holds the rank: `lower`
  and `upper`, both exact, and no `value`, since any one number would be read
  as the percentile. Past the last bound `upper` is `Infinity`.
- `estimate: true` adds `estimate`, a point between them interpolated as
  Prometheus' `histogram_quantile` does, named so nobody takes it for a
  measurement.
- A timer without `buckets` has no percentile: `invalid`, naming it.
- `p50` through `p99.9` and any rank between; `p99.9` ranks at 999 in 1,000
  exactly.

Five durations, 0.8, 41, 42, 43 and 44 ms, give these `p95`s, kept as a
gauge's samples or by a timer with the buckets `[1, 5, 10, 25, 50]`:

| Kept as                     | `p95`                                         |
|-----------------------------|-----------------------------------------------|
| a gauge                     | `value: 44`                                   |
| a timer                     | `lower: 25`, `upper: 50`                      |
| a timer, `estimate: true`   | `lower: 25`, `upper: 50`, `estimate: 48.4375` |
| one 0.4 ms, in such a timer | `lower: 0`, `upper: 1`, `estimate: 0.95`      |

The gauge's is the sample at rank 5 of 5. The timer's rank 5 falls in the
bucket from 25 to 50, and its estimate is 3.75 of that bucket's 4 samples into
it; one duration below the first bound is somewhere from 0 to 1.

An estimate is only as close as the bounds around it: put bounds where the
answer matters, around the 50 ms a page must load in. A program that shows
one number shows `value` when there is one and the estimate, marked as one,
when there is not; in Go `Value` is `nil` for a timer's percentile, never 0.

## Groups

Without `by`, every series of the name is one group and one answer. `by` keeps
the labels it names; `without` keeps all but those; `without: []` keeps every
series apart.

Each series' bucket is computed exactly, then joined, and the group's value
rounded once: counts, sums and increases add; `avg` divides every sample's sum
by every sample's count, never averaging averages; `p95` ranks every sample of
the group, or adds a timer's bucket counts before it ranks. `last` adds up
too: the last of several queues' depths is their total at the bucket's end.

```ts
const { groups: backlog } = await store.metrics.aggregate({ name: 'queue_depth', since: '1d', every: '1h', op: 'last' })
```

```python
backlog = (await store.metrics.aggregate(name="queue_depth", since="1d", every="1h", op="last")).groups
```

```go
backlog, err := metrics.Aggregate(ctx, store, metrics.Query{Name: "queue_depth", Since: 24 * time.Hour, Every: time.Hour, Op: metrics.Last})
```

```rust
let backlog = stats.query().name("queue_depth").since(Duration::from_hours(24)).every(Duration::from_hours(1)).aggregate(Op::Last)?.groups;
```

## A heatmap

```ts
const { groups: heat } = await store.metrics.aggregate({ name: 'http_request_ms_bucket', since: '1h', every: '1m', op: 'increase', by: ['le'] })
```

```python
heat = (await store.metrics.aggregate(name="http_request_ms_bucket", since="1h", every="1m", op="increase", by=["le"])).groups
```

```go
heat, err := metrics.Aggregate(ctx, store, metrics.Query{Name: "http_request_ms_bucket", Since: time.Hour, Every: time.Minute,
	Op: metrics.Increase, By: []string{"le"}})
```

```rust
let heat = stats.query().name("http_request_ms_bucket").since(Duration::from_hours(1)).every(Duration::from_mins(1)).by(&["le"]).aggregate(Op::Increase)?.groups;
```

How many durations fell under each bound, each minute: a timer's buckets are
counters, one a bound.

## Fast over long ranges

The store packs a series' samples into blocks where whole hours begin, each
with an exact summary. A bucket that holds whole blocks answers from their
summaries, so a month drawn by the hour decodes almost nothing; a bucket finer
than a block, and a gauge's percentile always, decode its samples.
[`explain`](reading.md#cost-before-it-runs) says what a query will cost.

## Aggregate in pages

A year of one-minute buckets is 525,600 buckets per group, more than one
aggregate may answer. Give `limit`, and `aggregate` returns a page of that many
buckets at most, with `next` when there is more:

```ts
const query = { name: 'requests_total', since: '365d', every: '1m', op: 'rate', limit: 10_000 } as const
const page = await store.metrics.aggregate(query)
const more = await store.metrics.aggregate({ ...query, after: page.next })

for await (const group of page) chart.appendBuckets(group.labels, group.buckets) // this page, then every page after it
```

```python
page = await store.metrics.aggregate(name="requests_total", since="365d", every="1m", op="rate", limit=10_000)

async for group in page:  # this page, then every page after it
    chart.append_buckets(group.labels, group.buckets)
```

```go
q := metrics.Query{Name: "requests_total", Since: 365 * 24 * time.Hour, Every: time.Minute, Op: metrics.Rate, Limit: 10_000}
page, err := metrics.Aggregate(ctx, store, q)

for group, err := range page.All(ctx) { // this page, then every page after it
	chart.AppendBuckets(group.Labels, group.Buckets)
}
```

```rust
let year = stats.query().name("requests_total").since(Duration::from_hours(365 * 24)).every(Duration::from_mins(1));
let page = year.limit(10_000).aggregate(Op::Rate)?;
for group in page.all() {
    chart.append_buckets(&group?); // this page, then every page after it
}
```

- The answer goes group by group, each group's buckets in time order. A page
  ends only between two whole buckets. A group that a page cuts has
  `more: true`, and the next page starts with the rest of its buckets.
- Each bucket is computed from every series of its group, as without
  `limit`. A `rate` or an `increase` in the first bucket of a page starts from
  the sample before it, and the edge of a page never makes a bucket
  `partial`.
- A bucket is never split. If one bucket alone passes a bound, such as a
  percentile over a year without `every`, the aggregate fails with a `limit`
  error.
- Pages don't lift the 1,000 series that a query may find, because a bucket
  needs every series of its group at once. Use `where` to find fewer.
- `next`, walking a page and what happens to writes between pages work as
  for [a read in pages](reading.md#read-a-long-range-in-pages).

## Samples you don't store

A Rust program that reads numbers live and never keeps them, such as rows of
another database or a payments API, can aggregate them without a store. Call
`query` on a `Series` you built in memory. It returns the same buckets that a
store's query returns for the same samples, because the same code counts
them, and it writes nothing to disk.

```rust
use tinystore::metrics::{self, ExactSum, Op, Series};

let requests = Series::counter("requests").sample(t0, 100.0).sample(t0 + Duration::from_secs(10), 150.0)
    .sample(t0 + Duration::from_secs(20), 20.0); // the process restarted
let minute = requests.query().from(t0).to(t0 + Duration::from_mins(1)).aggregate(Op::Increase)?;
// minute.buckets[0]: value 70, resets 1, as a store's query of these samples gives

let fleet = metrics::query(&[web1, web2]).calendar(Calendar::Day, "Europe/Moscow").aggregate(Op::Sum)?;
// one group: each series summed exactly, then the group, rounded once

let total: ExactSum = amounts.iter().copied().collect();
let paid = total.value(); // 1.0 for ten amounts of 0.1, where a float sum gives 0.9999999999999999
```

- `query` takes `from`, `to`, `every`, `calendar` and `estimate`, as a store's
  query does. Without `from` and `to`, the range runs from the first sample to
  the last.
- The series that you pass are one group, which you choose. They must share a
  name and a kind. The answer's labels are the ones that every series has.
- A bucket is `partial` against your machine's clock, as a store's query is
  against the store's.
- `ExactSum` is the sum alone: `add`, `sub`, `merge` for a group of sums, and
  `value`, rounded once.
- Only Rust has it so far. Bun, Node, Python and Go reach the metrics engine
  through a store.

## Errors

| Error     | When                                                                                                                                 |
|-----------|--------------------------------------------------------------------------------------------------------------------------------------|
| `invalid` | an `op` the kind has not; a percentile of a timer without buckets; a gauge bucket holding a NaN or an infinity; a counter below zero |
| `invalid` | samples you hold: series of two names or kinds, or two samples of one series in the same millisecond                                 |
| `limit`   | without `limit`, past 100,000 buckets; a bound of the query, which it names; one bucket that alone passes a bound                    |

## See also

- [Metrics](README.md): the engine's overview.
- [Instruments](instruments.md): what writes these series.
- [How logs and metrics pack](../../handbook/inside/how-logs-and-metrics-pack.md)
  in the Handbook.
