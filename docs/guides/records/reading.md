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

There is no `read`: `list` reads a page, `all` walks every page.

## One event, every way to find it

A webhook that arrived from GitHub for the target `home`:

```ts
await store.events('webhooks').append('github', { requestId: 'r-81', status: 200 }, { context: { target: 'home' } })
```

```python
await store.events("webhooks").append("github", {"requestId": "r-81", "status": 200}, context={"target": "home"})
```

```go
webhooks, err := records.OpenEvents(store, "webhooks")
_, err = webhooks.Append(ctx, "github", records.Fields{"requestId": "r-81", "status": 200},
	records.Context(records.Fields{"target": "home"}))
```

```rust
let webhooks = store.events("webhooks").open()?;
webhooks.event("github").context(&json!({ "target": "home" })).append(&json!({ "requestId": "r-81", "status": 200 }))?;
```

is read back as:

```json
{
  "at": "2026-10-21T20:24:49.322Z",
  "stream": "webhooks",
  "name": "github",
  "fields": { "requestId": "r-81", "status": 200 },
  "context": { "target": "home" }
}
```

| Finds it                       | Finds nothing              |
|--------------------------------|----------------------------|
| `streams: ['webhooks']`        | `streams: ['github']`      |
| `names: ['github']`            |                            |
| `where: { requestId: 'r-81' }` |                            |
| `where: { target: 'home' }`    |                            |
| `where: { status: 200 }`       | `where: { status: '200' }` |
| `search: 'r-81'`               |                            |

`github` is the event's name, not its stream. `where` finds a key of
`context` as it finds one of `fields`, and a value by the JSON it was written
as, a number as a number.

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

- Every condition given holds. Events come in the order of their times.
  [Events with the same time](#events-with-the-same-time) says how events
  that share a time are ordered.
- `where` finds a key at the top of `fields` or `context`, by the JSON its
  value was written as: `42` finds `42`, and neither `'42'` nor `42.0`. A
  key written twice, as another program's line may write it, is found by
  its last value, the one the event reads back with.
- `search` finds a phrase in the message, the name, and a value at any depth.
  It does not look at keys; `fieldNames` lists those.
- A condition given as `undefined` is `invalid`, never left out: a trace id
  that was not there must not read every event.

The time, the stream, the level, the trace and a `where` on a value that
rarely repeats, a request's id, skip the blocks that cannot match, as
[Reading only what can
match](../../handbook/inside/how-logs-and-metrics-pack.md#reading-only-what-can-match)
explains. `search` reads every event the rest of the query leaves, so give it
a range.

## Pages

`list` gives one page, and `all` every page in turn. A page holds its
`limit` of events that match, or a few fewer where the next ones share a
time, which a page never splits. Only the last page holds fewer and has no
`next`. To fill a page, `list` reads past the events that match nothing:
`mine` above, one user's last 50 events, comes back in one call however many
other users' events lie between them. So a page of a rare value reads as much
of the range as `count` would, and skips blocks the same way. `since` is
counted once: a page asked with `after` reads the range its first page had.

## Events with the same time

Events often share a time to the nanosecond. For example, all the events of
one `append` that give no `at` take the time of the call:

```ts
await audit.append([{ name: 'cart.created' }, { name: 'item.added' }, { name: 'checkout' }])
const page = await audit.list({ since: '1m' }) // cart.created, item.added, checkout
```

```python
await audit.append([Event("cart.created"), Event("item.added"), Event("checkout")])
page = await audit.list(since="1m")  # cart.created, item.added, checkout
```

```go
_, err := audit.AppendMany(ctx, []records.Event{{Name: "cart.created"}, {Name: "item.added"}, {Name: "checkout"}})
page, err := audit.List(ctx, records.Query{Since: time.Minute}) // cart.created, item.added, checkout
```

```rust
audit.append_all([NewEvent::new("cart.created"), NewEvent::new("item.added"), NewEvent::new("checkout")])?;
let page = audit.list().since(Duration::from_mins(1)).page()?; // cart.created, item.added, checkout
```

- Events that share a time come in the order they were appended. This holds
  across appends and across streams, for [late events](#which-events-are-late)
  too, and before and after TinyStore packs and merges them. With `newest`,
  they come in the reverse order.
- Two appends that run at the same time come in the order the store wrote
  them, and every read gives that same order.
- Events that TinyStore v0.1.0 packed are the exception. They keep the order
  that v0.1.0 gave them, the same on every read, which is not always the
  order they were appended (contract NOT-12). Events appended after you
  upgrade come after them, in the order they were appended.

### Which events are late

TinyStore packs late events apart from the others, so that they don't force
it to rewrite what it packed, as [How logs and metrics
pack](../../handbook/inside/how-logs-and-metrics-pack.md) explains. A read
still gives them in the order they were appended, and a
[tail](tail-and-poll.md#tail) as they were written.

An event is late when its `at` is more than ten seconds behind the store's
clock, and also more than ten seconds behind the newest on-time event that
its stream had before it. These on-time events count as before it:

- the events earlier in the same `append`
- the events of the stream's earlier appends, until packing or a
  [`drop`](repair.md) leaves none of the stream's on-time events waiting to
  be packed. Packing that leaves some of them waiting doesn't reset this.

When the store opens, the earlier events are the stream's on-time events that
still wait to be packed, whatever appended them, and the ones already packed
stop counting. The same happens when a node becomes the home of `records.db`.
If an append runs at the same time as another append to its stream, or while
TinyStore packs or drops events of the stream, either may count as the earlier
one. Until the store opens again, an append that fails counts as none.

So an event appended without `at` is never late.

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
- When few events match, as with a search for a rare word, they read several
  pages at once on several threads. In Rust, `all` does the same.
- `top` gives a key's most frequent values, ten unless `limit` asks for more,
  1,000 at most. The key is a field's or a context's, or `stream`, `name` or
  `level`.
- `calendar` with `timeZone` (`time_zone` in Python, `Calendar` and
  `TimeZone` in Go, `.calendar(Calendar::Day, zone)` in Rust) gives a row for
  each local day, week from Monday or month in place of `every`, as
  [metrics' days](../metrics/aggregates.md#days-in-a-time-zone) do.
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

## What a stream's events carry

`fields` says which names a stream's events have and which keys their fields
hold, with how many events carry each and what kinds of value it holds. So a
program can draw a chart of a stream it has never seen, such as orders paid
per day summed by `amount`.

```ts
const orders = await store.events().fields('orders')
// eventNames: [{ name: 'order.paid', count: 100 }]
// fields:     [{ name: 'amount', kinds: ['float'], count: 100 },
//              { name: 'currency', kinds: ['text'], count: 100 },
//              { name: 'order_id', kinds: ['integer'], count: 100 },
//              { name: 'paid', kinds: ['bool'], count: 100 }]
// context:    []
```

```python
orders = await store.events().fields("orders")
# event_names: [NameCount("order.paid", 100)]
# fields:      [FieldInfo("amount", ["float"], 100), FieldInfo("currency", ["text"], 100),
#               FieldInfo("order_id", ["integer"], 100), FieldInfo("paid", ["bool"], 100)]
# context:     []
```

```go
orders, err := events.Fields(ctx, "orders")
// EventNames: [{Name: "order.paid", Count: 100}]
// Fields:     [{Name: "amount", Kinds: [KindFloat], Count: 100}, {Name: "currency", Kinds: [KindText], Count: 100},
//              {Name: "order_id", Kinds: [KindInteger], Count: 100}, {Name: "paid", Kinds: [KindBool], Count: 100}]
```

```rust
let orders = store.all_events()?.fields("orders")?;
// event_names: [NameCount { name: "order.paid", count: 100 }]
// fields:      [FieldInfo { name: "amount", kinds: [Kind::Float], count: 100 },
//               FieldInfo { name: "currency", kinds: [Kind::Text], count: 100 }, …]
```

- `eventNames / event_names / EventNames / event_names` lists the names of
  the stream's events, each with how many events have it.
- `kinds` lists each kind of value the key holds, in this order: `float`,
  `integer`, `text`, `bool`, `object`, `list` and `null`. `float` is a
  number with a fraction or an exponent, such as `12.5`, and `integer` is a
  whole number, such as `42`. A key that holds `12` in one event and `12.5`
  in another has both.
- `count` is how many events carry the key. An event that writes a key twice
  counts once.
- `context` lists the keys of the events' contexts in the same way, apart
  from their fields.
- The counts include every event the stream holds, the packed ones and the
  ones that wait to be packed. When `keep` removes old events, their counts
  go with them.
- A stream without events carries nothing, and a name that no stream can
  have fails with an invalid error.

TinyStore counts a segment's events when it packs them and keeps the counts
beside the segment, so a stream's packed events cost one small query
however many there are. The events that wait to be packed, at most an hour's
worth, are read from their rows, so `fields` takes longer on a stream that
has many events waiting.

> [!NOTE]
> **A store from 0.5.1 or before**
> Events that 0.5.1 or an earlier release packed have no counts. The store's
> maintenance counts them after it opens, as [0.6.0's
> notes](../../releases/v0.6.0.md#what-a-streams-events-carry) say, and until
> then `fields` leaves those events out.

## See also

- [Records](README.md): the engine's overview.
- [Traces](traces.md): one request's events across streams.
- [Tail and poll](tail-and-poll.md): events as they arrive.
- [`tinystore logs`](../cli/status-and-logs.md#logs): the same lines from a
  shell, beside the program or server that holds the directory.
