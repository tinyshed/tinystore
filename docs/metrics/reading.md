# Reading

Read the raw samples of one or more series, exactly as they were stored.
Find series by name, by labels, or by conditions such as "one of these
values" or "starts with". Use reads to draw a chart or to export data. To
compute totals and rates, use [aggregates](aggregates.md) instead.

## Read a series

```ts
const series = await store.metrics.read({ name: 'cpu', match: { host: 'web-1' }, since: '1h' })

for (const s of series) {
	console.log(s.labels, s.times.length) // times in unix milliseconds, values in a Float64Array
}
```

```python
series = await store.metrics.read(name="cpu", match={"host": "web-1"}, since="1h")

for s in series:
    print(s.labels, len(s.times))  # times in unix milliseconds, values in an array("d")
```

```go
results, err := stats.Read(ctx, metrics.Range{
	Name:  "cpu",
	Match: metrics.Labels{"host": "web-1"},
	Since: time.Hour,
})
for _, r := range results {
	fmt.Println(r.Series.Labels, len(r.Samples)) // each sample: At in unix milliseconds, Value
}
```

A read returns one result per matching series. In Bun and Python, a series
holds its samples as two columns, `times` and `values`, so a long read doesn't
create an object per sample.

A time range is `since`, a duration back from now, or `from` and `to` (`from_`
in Python). `to` is excluded, and without it the range is open.

## Find series

```ts
import { noneOf, oneOf, prefix } from 'tinystore'

const failing = await store.metrics.read({
	name: 'http_requests_total',
	since: '1h',
	where: { status: oneOf('500', '502', '503'), env: noneOf('dev'), host: prefix('api-') },
})
```

```python
failing = await store.metrics.read(
    name="http_requests_total",
    since="1h",
    where={"status": tinystore.one_of("500", "502", "503"), "env": tinystore.none_of("dev"), "host": tinystore.prefix("api-")},
)
```

```go
failing, err := stats.Read(ctx, metrics.Range{
	Name:  "http_requests_total",
	Since: time.Hour,
	Where: metrics.Where{
		"status": metrics.OneOf("500", "502", "503"),
		"env":    metrics.NoneOf("dev"),
		"host":   metrics.Prefix("api-"),
	},
})
```

| Condition | Matches a series whose label |
|---|---|
| `match: { host: 'web-1' }` | equals the value |
| `oneOf(…)` | equals one of the values |
| `noneOf(…)` | equals none of the values, or is missing |
| `prefix(…)` | starts with the text |

All conditions must match. A query needs a name, a `match`, a `oneOf` or a
`prefix` to find its series. `noneOf` alone is rejected, because it would have
to look at every series. There are no regular expressions.

Without a name, a `match` finds every series with those labels:
`read({ match: { host: 'web-1' } })` returns every metric of host `web-1`.

## Know the cost before you ask

```ts
const plan = await store.metrics.explain({ name: 'cpu', since: '30d' })
plan.series  // how many series it matches
plan.decoded // how many samples it would decode
plan.stops   // the limit it would hit, if any
```

```python
plan = await store.metrics.explain(name="cpu", since="30d")
```

```go
plan, err := stats.ExplainRead(ctx, metrics.Range{Name: "cpu", Since: 30 * 24 * time.Hour})
```

`explain` reads only the index and returns what the query would cost: the
series, blocks and bytes it reads, the samples it decodes, and the limit
error it would end with. It doesn't fetch or decode any samples.

## Limits

A query has separate limits for the series it matches, the blocks and bytes
it reads, the samples it decodes and the samples it returns. When a query
reaches one, it fails with a limit error that names the limit, how much the
query wanted and what the limit is. It never returns a smaller answer
silently.

| | |
|---|---|
| Series per query | 1,000 |
| Bytes read | 16 MiB |
| Samples decoded | 1,048,576 |
| Samples returned | 100,000 |

A query can lower these limits for itself. Raise them in the store's options.

## See also

- [Aggregates](aggregates.md): sums, rates and averages per time bucket.
- [metrics/README.md](../../metrics/README.md): the full contract of reads.
