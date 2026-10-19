# Reading

`store.events()` reads every stream; `store.events('audit')` reads one. A
query finds events by time, stream, name, level, trace, a field's value or
text anywhere in them, a page at a time. The same query counts them, finds a
key's most frequent values, and aggregates them by span and key.

```ts
const page = await store.events().list({ since: '1h', minLevel: 'warn' }) // { events, next }
const more = await store.events().list({ since: '1h', minLevel: 'warn', after: page.next })

for await (const event of store.events().all({ since: '24h', search: 'connection reset' })) show(event)
const mine = await audit.list({ where: { user: 42 }, newest: true, limit: 50 })
```

```python
page = await store.events().list(since="1h", min_level="warn")  # EventPage(events, next)
more = await store.events().list(since="1h", min_level="warn", after=page.next)

async for event in store.events().all(since="24h", search="connection reset"):
    show(event)
mine = await audit.list(where={"user": 42}, newest=True, limit=50)
```

```go
events, err := records.OpenAllEvents(store)
page, err := events.List(ctx, records.Query{Since: time.Hour, MinLevel: slog.LevelWarn})
more, err := events.List(ctx, records.Query{Since: time.Hour, MinLevel: slog.LevelWarn, After: page.Next})

for event, err := range events.All(ctx, records.Query{Since: 24 * time.Hour, Search: "connection reset"}) {
	show(event, err)
}
mine, err := audit.List(ctx, records.Query{Where: records.Where{"user": 42}, Newest: true, Limit: 50})
```

```rust
let page = store.all_events()?.list().since(Duration::from_hours(1)).min_level(Level::WARN).page()?;
for event in store.all_events()?.list().since(Duration::from_hours(24)).search("connection reset").all() {
    show(event?);
}
let mine = audit.list().filter("user", &42).newest().limit(50).page()?;
```

## A query

| In a query   | Finds                                                             |
|--------------|-------------------------------------------------------------------|
| `since`      | the span before now, `'1h'`; or `from` and `to`                   |
| `from`, `to` | the range's start, included, and its end, excluded                |
| `streams`    | any of these streams; `store.events()`'s alone                    |
| `names`      | any of these names: `['log']` is the lines                        |
| `minLevel`   | this level and worse; an event without a level is not found       |
| `traceId`    | one trace's events, every stream                                  |
| `where`      | each key with this value, in `fields` or in `context`             |
| `search`     | this text in the message, the name or any value, its case ignored |
| `newest`     | newest first; oldest first without it                             |
| `limit`      | the events of a page: 1,000, and 10,000 at most                   |
| `after`      | a page's `next`, with the rest of the query as it was             |

- Every condition given holds. Events come in the order of their times, those
  of one time in the order they were written.
- `where` finds a key at the top of `fields` or `context`, by the JSON its
  value was written as: `42` finds `42`, and neither `'42'` nor `42.0`.
- `search` finds a phrase in the message, the name, and a value at any depth.
  It does not look at keys; `fieldNames` lists those.
- A condition given as `undefined` is `invalid`, never left out: a trace id
  that was not there must not read every event.

The time, the stream, the level, the trace and a `where` on a value that
rarely repeats, a request's id, skip the blocks that cannot match without
reading them. `search` reads every event the rest of the query leaves, so give
it a range.

## Pages

`list` gives one page from one moment of the file, and `all` every page in
turn. A page may hold fewer events than its `limit`, or none, and still have
a `next`: a read stops at what one read may fetch rather than failing, a
`search` over much text soonest. The last page is the one without a `next`.
`since` is counted once: a page asked with `after` reads the range its first
page had.

## Count, top and aggregate

```ts
const errors = await store.events().count({ since: '1h', minLevel: 'error' }) // 42

const routes = await store.events().top('route', { since: '1h', minLevel: 'error' })
// [{ value: '/notes', count: 12 }, { value: '/login', count: 7 }, …]

const hourly = await store.events().aggregate({ since: '24h', every: '1h', by: ['level'] })
// [{ at, count: 3, by: { level: 'error' } }, { at, count: 41, by: { level: 'warn' } }, …]

const latency = await api.aggregate({ since: '24h', names: ['request'], every: '5m', stats: ['duration'] })
// [{ at, count: 310, stats: { duration: { samples: 287, sum: 12772, min: 3, max: 1200, avg: 44.5 } } }, …]
```

```python
errors = await store.events().count(since="1h", min_level="error")
routes = await store.events().top("route", since="1h", min_level="error")  # [Top(value, count), …]
hourly = await store.events().aggregate(since="24h", every="1h", by=["level"])
latency = await api.aggregate(since="24h", names=["request"], every="5m", stats=["duration"])
```

```go
errors, err := events.Count(ctx, records.Query{Since: time.Hour, MinLevel: slog.LevelError})
routes, err := events.Top(ctx, "route", records.Query{Since: time.Hour, MinLevel: slog.LevelError})
hourly, err := events.Aggregate(ctx, records.Query{Since: 24 * time.Hour, Every: time.Hour, By: []string{"level"}})
```

```rust
let errors = store.all_events()?.list().since(Duration::from_hours(1)).min_level(Level::ERROR).count()?;
let routes = store.all_events()?.list().since(Duration::from_hours(1)).top("route")?;
let hourly = store
    .all_events()?
    .list()
    .since(Duration::from_hours(24))
    .aggregate(&Aggregate::new().every(Duration::from_hours(1)).by(["level"]))?;
```

- Each takes a query as `list` does, without `newest`, `limit` and `after`.
- `top` gives a key's most frequent values, ten unless `limit` asks for more,
  1,000 at most. The key is a field's or a context's, or `stream`, `name` or
  `level`.
- `aggregate` gives a row for each span of `every` and each value of the keys
  in `by`, with its `count`. A span without events has no row. `at` is where
  a row's span begins, counted from 1970 in UTC.
- `stats` names fields, and a row gives each one's `samples`, `sum`, `min`,
  `max` and `avg` over the events whose value there is a number. `distinct`
  gives how many different values a field has.
- A sum of integers is exact, a `bigint` past 2⁵³.
- An answer is whole or an error: past 10,000 rows, or 64 MiB of different
  values held for `top` or `distinct`, it is `limit`, never a smaller answer.
  Ask for a shorter range or a wider `every`.

## What a store holds

```ts
const streams = await store.events().streams() // [{ name: 'api', keep: '14d', retainedFrom, newest }, …]
const keys = await store.events('api').fieldNames({ since: '1h' }) // ['ms', 'requestId', 'route', 'user']
```

```python
streams = await store.events().streams()
keys = await store.events("api").field_names(since="1h")
```

```go
streams, err := events.Streams(ctx)
keys, err := api.FieldNames(ctx, records.Query{Since: time.Hour})
```

```rust
let streams = store.all_events()?.streams()?;
let keys = api.list().since(Duration::from_hours(1)).field_names()?;
```

`streams` lists every stream with events, its `keep`, where what it keeps
begins, and its newest event. `fieldNames` gives the keys events hold, which a
page's filter menu offers. A key's values are `top`'s, and a stream's names
`top('name')`'s.

## From a shell

`tinystore logs ./data --since 1h --level warn` reads the same lines beside the
program or server that holds the directory, and `tinystore logs ./data -f`
follows them as they arrive.

## See also

- [Records](README.md): the engine's overview.
- [Traces](traces.md): one request's events across streams.
- [Tail and poll](tail-and-poll.md): events as they arrive.
