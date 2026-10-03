# Reading

Read records back by time, stream, level, fields and text, one page at a time
or all of them in a loop. A query reads only the parts of the file that can
match, so finding one request among millions of records takes milliseconds.

## Read a page

```ts
const page = await store.records.scan({
	since: '1h',
	streams: ['api'],
	minLevel: 'warn',
	newest: true,
	limit: 100,
})

for (const record of page.items) {
	console.log(record.level, record.body)
}
const more = page.next ? await store.records.scan({ since: '1h', streams: ['api'], minLevel: 'warn', newest: true, after: page.next }) : undefined
```

```python
page = await store.records.scan(since="1h", streams=["api"], min_level="warn", newest=True, limit=100)

for record in page.items:
    print(record.level, record.body)
if page.next:
    more = await store.records.scan(since="1h", streams=["api"], min_level="warn", newest=True, after=page.next)
```

```go
warn := slog.LevelWarn
page, err := logs.Scan(ctx, records.Query{
	Since: time.Hour, Streams: []string{"api"}, MinLevel: &warn, Newest: true, Limit: 100,
})

for _, record := range page.Records {
	fmt.Println(record.At, record.Stream, record.Name)
}
if page.More {
	page, err = logs.Scan(ctx, page.Next)
}
```

To get the next page, pass the same query with `after: page.next`. In Go,
`page.Next` is already the next query. The next page continues the same time
range, even if you ask for it an hour later.

A page never splits records that have the same timestamp, so the next page
starts exactly where this one ended, without losing or repeating a record.

## Read everything

```ts
for await (const record of store.records.all({ since: '24h', streams: ['worker'] })) {
	handle(record)
}
```

```python
async for record in store.records.all(since="24h", streams=["worker"]):
    handle(record)
```

```go
for record, err := range logs.All(ctx, records.Query{Since: 24 * time.Hour, Streams: []string{"worker"}}) {
	if err != nil {
		return err
	}
	handle(record)
}
```

`all` reads one page after another for you. If you stop the loop early, it
reads no further pages.

## Filter

| Filter | Bun | Python | Go |
|---|---|---|---|
| the last hour | `since: '1h'` | `since="1h"` | `Since: time.Hour` |
| a time range | `from`, `to` | `from_`, `to` | `From`, `To` |
| streams | `streams: ['api']` | `streams=["api"]` | `Streams: []string{"api"}` |
| event names | `names: ['click']` | `names=["click"]` | `Names: []string{"click"}` |
| a level and above | `minLevel: 'warn'` | `min_level="warn"` | `MinLevel: &warn` |
| a trace | `traceId` | `trace_id` | `TraceID` |
| attributes | `attrs: { route: '/users' }` | `attrs={"route": "/users"}` | `Attrs: []records.Field{…}` |
| context | `context: { session: id }` | `context={"session": id}` | `Context: []records.Field{…}` |
| text in the body or name | `search: 'timeout'` | `search="timeout"` | `Search: "timeout"` |
| newest first | `newest: true` | `newest=True` | `Newest: true` |

All filters must match. A record without a level doesn't match a level
filter. Attributes and context match by key and exact value.

## Search text

```ts
const resets = await store.records.scan({ since: '1h', search: 'connection reset' })
```

```python
resets = await store.records.scan(since="1h", search="connection reset")
```

```go
resets, err := logs.Scan(ctx, records.Query{Since: time.Hour, Search: "connection reset"})
```

`search` finds records whose body or name contains the text, ignoring case.
There is no word index yet, so a search reads every record that the other
filters leave. Narrow it with a time range and a stream. A search over a lot
of text ends a page early when it reaches its budget, and `next` continues it.

## How a query stays fast

Records are stored in blocks of up to 1,024 records, sorted by time. A query
skips every block that can't match:

- The time index finds only the blocks in the range.
- Each block knows the levels it contains.
- A block has a filter of its trace ids, and of each id-like attribute, such as
  a request id.

Finding one request id among 1.32 million production log lines read 11 blocks
and took 6 ms, in a Linux container on a Ryzen 7 7700
([report](https://github.com/tinyshed/research/blob/main/tinystore/reports/records-engine-2026-09-25.md)).

## Limits and defaults

| | |
|---|---|
| A page | 1,000 records by default, at most 10,000 |
| A page's budget | the blocks, bytes and records a read may decode |
| A search text | 1 KiB |

If more records share one timestamp than a page can hold, `scan` fails with a
limit error.

## See also

- [Traces](traces.md): read every record of one request.
- [Following](following.md): process new records in order.
- [records/README.md](../../records/README.md): the full contract of reads.
