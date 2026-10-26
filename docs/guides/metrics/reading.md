# Reading

`read` gives series as they were written: each with its name, labels and
samples, every value bit for bit. `latest` gives each series' newest sample.
`names`, `labelNames` and `labelValues` list what a store holds, for the
menus of a dashboard, without reading a sample.

```ts
const { series } = await store.metrics.read({ name: 'cpu', where: { host: 'web-1' }, since: '1h' })
for (const { labels, times, values } of series) chart.line(labels.host, times, values)
```

```python
series = (await store.metrics.read(name="cpu", where={"host": "web-1"}, since="1h")).series
for s in series:
    chart.line(s.labels["host"], s.times, s.values)
```

```go
page, err := metrics.Read(ctx, store, metrics.Query{Name: "cpu", Where: metrics.Where{"host": "web-1"}, Since: time.Hour})
for _, s := range page.Series {
	chart.Line(s.Labels["host"], s.Samples)
}
```

```rust
let stats = store.metrics()?;
let series = stats.query().name("cpu").filter("host", "web-1").since(Duration::from_hours(1)).read()?.series;
```

A series comes back with its samples as two columns: `times`, unix
milliseconds in a `Float64Array`, which a chart and `new Date` take, and
`values`, a `Float64Array` whose bytes keep every value's bits. Python gives
the same two arrays; Go and Rust a slice of samples. A NaN in `values` is a
value that was stored, never a missing one.

## Conditions

```ts
const { series: failing } = await store.metrics.read({
	name: 'http_requests_total',
	since: '1h',
	where: { status: ['500', '502'], env: { not: 'dev' }, host: { prefix: 'api-' } },
})
const { series: host } = await store.metrics.read({ where: { host: 'web-1' }, since: '10m' }) // every name of one host
```

```python
failing = (await store.metrics.read(
    name="http_requests_total",
    since="1h",
    where={"status": ["500", "502"], "env": {"not": "dev"}, "host": {"prefix": "api-"}},
)).series
host = (await store.metrics.read(where={"host": "web-1"}, since="10m")).series
```

```go
failing, err := metrics.Read(ctx, store, metrics.Query{Name: "http_requests_total", Since: time.Hour,
	Where: metrics.Where{"status": metrics.OneOf("500", "502"), "env": metrics.Not("dev"), "host": metrics.Prefix("api-")}})
host, err := metrics.Read(ctx, store, metrics.Query{Where: metrics.Where{"host": "web-1"}, Since: 10 * time.Minute})
```

```rust
let failing = stats
    .query()
    .name("http_requests_total")
    .since(Duration::from_hours(1))
    .filter("status", ["500", "502"])
    .filter("env", Not("dev"))
    .filter("host", Prefix("api-"))
    .read()?
    .series;
```

| In a query           | Finds                                                  |
|----------------------|--------------------------------------------------------|
| `name`               | the series of this name; a timer's name, all it writes |
| `{ host: 'a' }`      | a label equal to this value                            |
| `['500', '502']`     | equal to one of these                                  |
| `{ not: 'dev' }`     | equal to none of these, or absent                      |
| `{ prefix: 'api-' }` | starting with this text                                |
| `since`              | the span before now, `'1h'`; or `from` and `to`        |

- Every condition holds. A query needs a `name`, or a label's value, list or
  prefix, to find its series: `not` alone is `invalid`, since it would read
  every series. There are no regular expressions.
- A number in `where` is its text: `{ status: 200 }` finds `"200"`.
- A condition given as `undefined` is `invalid`, never left out.
- A query is data: the same object is kept in a dashboard's settings, sent
  over the wire, and read the same in every language.

## The newest value

```ts
const cpu = await store.metrics.latest({ name: 'cpu', since: '5m' }) // [{ name, labels, at, value }, …]
```

```python
cpu = await store.metrics.latest(name="cpu", since="5m")
```

```go
cpu, err := metrics.Latest(ctx, store, metrics.Query{Name: "cpu", Since: 5 * time.Minute})
```

```rust
let cpu = stats.query().name("cpu").since(Duration::from_mins(5)).latest()?;
```

`latest` gives one sample a series, the newest in the range. A series without
one is left out, so the range bounds how stale an answer may be: a host that
went down leaves the answer rather than showing its last value for ever. It
reads one small piece of each series, however long the range.

## What a store holds

```ts
const page = await store.metrics.names({ prefix: 'http_' })
// { names: [{ name, kind, seriesCount, unit, help, keep, retainedFrom, newest }, …], next }
const { labels } = await store.metrics.labelNames('http_requests_total') // ['route', 'status']
const { values } = await store.metrics.labelValues('http_requests_total', 'route', { prefix: '/api' })

await store.metrics.describe('cpu', { unit: '%', help: 'Busy time of a core.' })
```

```python
page = await store.metrics.names(prefix="http_")
labels = (await store.metrics.label_names("http_requests_total")).labels
values = (await store.metrics.label_values("http_requests_total", "route", prefix="/api")).values

await store.metrics.describe("cpu", unit="%", help="Busy time of a core.")
```

```go
page, err := metrics.Names(ctx, store, metrics.Prefix("http_"))
labels, err := metrics.LabelNames(ctx, store, "http_requests_total")
values, err := metrics.LabelValues(ctx, store, "http_requests_total", "route", metrics.Prefix("/api"))

err = metrics.Describe(ctx, store, "cpu", metrics.Description{Unit: "%", Help: "Busy time of a core."})
```

```rust
let page = stats.names().prefix("http_").page()?;
let labels = stats.label_names("http_requests_total").page()?.labels;
let routes = stats.label_values("http_requests_total", "route").prefix("/api").page()?.values;

stats.describe("cpu", Description { unit: "%".into(), help: "Busy time of a core.".into() })?;
```

None of them reads a sample: the file keeps what they give beside the series,
so a dashboard's menus stay fast however much the store holds. Pages are
1,000 entries unless `limit` says; `after: page.next` reads the next.

## Cost before it runs

```ts
const plan = await store.metrics.explain({ name: 'cpu', since: '30d', every: '1h', op: 'avg' })
// { series: 40, bytes: 1310720, decoded: 960 }
```

```python
plan = await store.metrics.explain(name="cpu", since="30d", every="1h", op="avg")
```

```go
plan, err := metrics.Explain(ctx, store, metrics.Query{Name: "cpu", Since: 30 * 24 * time.Hour, Every: time.Hour, Op: metrics.Avg})
```

```rust
let plan = stats.query().name("cpu").since(Duration::from_hours(30 * 24)).every(Duration::from_hours(1)).explain(Op::Avg)?;
```

A query is bounded before it starts: series, bytes read, samples decoded and
the size of the answer are separate budgets, and a query past one is `limit`,
naming it, never a smaller answer. `explain` says how many series, bytes and
decoded samples a `read` or an `aggregate` would spend, and in `exceeds` the
bound it would stop at, `answer` included.

| Bound           | A query's limit            |
|-----------------|----------------------------|
| series          | 1,000                      |
| bytes read      | 16 MiB                     |
| samples decoded | 1,048,576                  |
| an answer       | 100,000 samples or buckets |

## Removing a series

```ts
await store.metrics.delete({ name: 'cpu', labels: { host: 'web-1' } }) // true if it held that series
```

```python
await store.metrics.delete(name="cpu", labels={"host": "web-1"})
```

```go
held, err := metrics.Delete(ctx, store, "cpu", metrics.Labels{"host": "web-1"})
```

```rust
let held = stats.delete("cpu", [("host", "web-1")])?;
```

`delete` removes the one series of exactly these labels and all its samples.
The labels are always given, so a delete never reads as one of every series of
a name. A series whose bytes changed under it reads `corrupt`, stops taking
samples, and is listed by `damaged`; `delete` removes it.

## See also

- [Metrics](README.md): the engine's overview.
- [Aggregates](aggregates.md): rates, averages and percentiles by bucket.
