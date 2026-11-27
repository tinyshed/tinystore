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
| `where`      | each key with a value, one of a list, `{ not }` or `{ prefix }`   |
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
- A list in `where` means any of its values, `{ not }` none of them, and
  `{ prefix }` a text that starts with it: [A where](#a-where). A database's
  statement means the values its one column gives: [A where kept to a
  database's statement](#a-where-kept-to-a-databases-statement).
- `search` finds a phrase in the message, the name, and a value at any depth.
  It does not look at keys; `fieldNames` lists those.
- A condition given as `undefined` is `invalid`, never left out: a trace id
  that was not there must not read every event. So is a field no query has,
  in TypeScript and Python, such as `name` written for `names`: the error
  names the field, and the one it was likely meant to be. Go and Rust
  refuse it when the program compiles.

The time, the stream, the level, the trace and a `where` on a value that
rarely repeats, a request's id, skip the blocks that cannot match, as
[Reading only what can
match](../../handbook/inside/how-logs-and-metrics-pack.md#reading-only-what-can-match)
explains. `search` reads every event the rest of the query leaves, so give it
a range.

## A where

```ts
await api.aggregate({ names: ['call'], since: '1d', by: ['op'], where: { op: ['grading.works', 'drafts.get'] } })
await api.list({ since: '1d', where: { userId: [7, 12, 31], route: { prefix: '/admin' }, env: { not: 'test' } } })
await api.list({ since: '1d', where: { tags: [['a', 'b']] } }) // the list ['a', 'b'] itself
```

```python
await api.aggregate(names=["call"], since="1d", by=["op"], where={"op": ["grading.works", "drafts.get"]})
await api.list(since="1d", where={"userId": [7, 12, 31], "route": {"prefix": "/admin"}, "env": {"not": "test"}})
await api.list(since="1d", where={"tags": [["a", "b"]]})  # the list ["a", "b"] itself
```

```go
api.Aggregate(ctx, records.Query{Names: []string{"call"}, Since: 24 * time.Hour, By: []string{"op"},
	Where: records.Where{"op": records.OneOf("grading.works", "drafts.get")}})
api.List(ctx, records.Query{Since: 24 * time.Hour,
	Where: records.Where{"userId": []int{7, 12, 31}, "route": records.Prefix("/admin"), "env": records.Not("test")}})
api.List(ctx, records.Query{Since: 24 * time.Hour, Where: records.Where{"tags": [][]string{{"a", "b"}}}})
```

```rust
use tinystore::{Not, Prefix};
let day = Duration::from_hours(24);
api.list().since(day).filter("op", &["grading.works", "drafts.get"]).aggregate(&Aggregate::new().by(["op"]))?;
api.list().since(day).filter("userId", &[7, 12, 31]).filter("route", Prefix("/admin")).filter("env", Not("test")).page()?;
api.list().since(day).filter("tags", &[["a", "b"]]).page()?; // the list ["a", "b"] itself
```

| A key's condition         | Finds an event whose value there is                       |
|---------------------------|-----------------------------------------------------------|
| a value: `'drafts.get'`   | that value, by the JSON it was written as                 |
| a list: `[7, 12, 31]`     | any of them, 1 to 1,000 values                            |
| `{ not: 'test' }`         | none of them, a value or a list, or an event without key  |
| `{ prefix: '/admin' }`    | a text that starts with it; a number or a bool never does |
| a list in a list: `[[…]]` | that list, or that object, itself                         |

- Go writes `records.OneOf(…)`, `records.Not(…)` and `records.Prefix(…)`,
  and a bare slice means the same as `OneOf`. Rust writes `&[…]` for a list
  and `tinystore::Not(…)` and `tinystore::Prefix(…)`, the words metrics'
  conditions use.
- Each item matches by its exact JSON, as a single value does: `[42, '42']`
  finds `42` and `'42'`, and not `42.0`.
- An empty list, a prefix of nothing, and any other object are `invalid`.
  A list or an object as the value itself goes inside a list.
- Each value of a list, and a prefix, is at most 4 KiB.
- `{ not }` reads more than the other conditions, since a block without the
  key may hold what it finds: give it a range or another condition.

> [!NOTE]
> **What a list meant before**
> A list in `where` was compared as the value itself, so `{ op: ['a', 'b'] }`
> found only an event whose `op` was that list. Write `{ op: [['a', 'b']] }`
> for that. A server of a release before conditions refuses a list,
> `{ not }` and `{ prefix }` as `unimplemented`, rather than match nothing.

## A where kept to a database's statement

What teachers use: the events of the people whose id is in the teachers'
table, counted by the store rather than gathered for everybody and kept to
the teachers in the program.

```ts
const teachers = db.statement`select id from teachers` // bound to its database; nothing runs yet
await events.count({ since: '1d', where: { userId: teachers } })
await events.aggregate({ since: '7d', by: ['name'], distinct: ['userId'], where: { userId: teachers } })
await events.list({ since: '1d', where: { userId: { not: teachers } } })
```

```python
teachers = db.statement("select id from teachers")  # bound to its database; nothing runs yet
await events.count(since="1d", where={"userId": teachers})
await events.aggregate(since="7d", by=["name"], distinct=["userId"], where={"userId": teachers})
await events.list(since="1d", where={"userId": {"not": teachers}})
```

```go
teachers := db.Statement("select id from teachers") // bound to its database; nothing runs yet
events.Count(ctx, records.Query{Since: 24 * time.Hour, Where: records.Where{"userId": teachers}})
events.Aggregate(ctx, records.Query{Since: 7 * 24 * time.Hour, By: []string{"name"}, Distinct: []string{"userId"},
	Where: records.Where{"userId": teachers}})
events.List(ctx, records.Query{Since: 24 * time.Hour, Where: records.Where{"userId": records.Not(teachers)}})
```

```rust
let (teachers, day) = (db.statement("select id from teachers"), Duration::from_hours(24)); // nothing runs yet
events.list().since(day).filter("userId", &teachers).count()?;
events.list().since(day * 7).filter("userId", &teachers).aggregate(&Aggregate::new().by(["name"]).distinct(["userId"]))?;
events.list().since(day).filter("userId", Not(&teachers)).page()?;
```

- The statement is read once, as the query starts, on a reader of its
  database, and its one column's values are the list a key keeps to;
  `{ not }` keeps to none of them, or no such key. A page asked later with
  `after` reads it again.
- It is not read in one moment with the events: they lie in records.db and
  the rows in the database's file, and the store promises no moment across
  two files.
- Its values match as a list's do, by the JSON an event's value was written
  as:

  | The statement gives | Finds                             |
  |---------------------|-----------------------------------|
  | the integer `7`     | `7`                               |
  | the text `'7'`      | `"7"`, and never `7`              |
  | the real `7.0`      | `7.0`, and not `7`; `0.5` `0.5`   |
  | `null`              | nothing                           |
  | a blob              | refused, `invalid`: select a text |

- It gives 100,000 rows at most; past them the query is `limit`. A statement
  that writes, or gives other than one column, is `invalid`.
- `count`, `list`, `top`, `largest`, `aggregate` and `delete` take one, and a
  delete reads it once, as it starts, for every segment it changes. A tail
  lives on and refuses one, `invalid`.
- A stream lives in records.db and belongs to no database, so a statement
  says whose it is: a bare `` sql`…` `` in a stream's `where` is `invalid`,
  where a table's index takes one, since an index lives in its database.
- A statement of more than 100 values passes no block by its filter, which
  says maybe of one value in a hundred it does not hold; the read still
  looks a value up by its hash, once an event.

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

The events of one `append` that give no `at` take the time of the call, 100
nanoseconds apart in the order you list them: the first takes the call's
time, the next 100 ns more, and so on. 100 ns is the finest step a clock
keeps on every platform, Windows included, so each event reads back with the
time it was given. An event that gives its own `at` keeps it. So a list reads
back in the order you appended it, and a page can end between any two of its
events, however many there are:

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

Events still share a time to the nanosecond when you give them the same
`at`, or when two appends that run at the same time give theirs the same
times.

- Events that share a time come in the order they were appended. This holds
  across appends and across streams, for [late events](#which-events-are-late)
  too, and before and after TinyStore packs and merges them. With `newest`,
  they come in the reverse order.
- Events of one time from two appends that run at the same time come in the
  order the store wrote them, and every read gives that same order.
- A page never splits a time, so more events of one time than its `limit`
  fail the page with a `limit` error. Give such events times apart, or ask
  for a larger `limit`.
- Events that TinyStore v0.1.0 packed are the exception. They keep the order
  that v0.1.0 gave them, the same on every read, which is not always the
  order they were appended (contract NOT-12): in one stream a late event may
  come before or after the stream's other events of its time, and events of
  different streams come in the order of the blocks they were packed in.
  Events appended after you upgrade come after them, in the order they were
  appended.

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
  1,000 at most. The key is a field's or a context's, or `stream`, `name`,
  `level` or `message`.
- `message` is the event's message in `top`, `by` and `distinct`. An event
  without one gives a field or a context key named `message` instead.
- `calendar` with `timeZone` (`time_zone` in Python, `Calendar` and
  `TimeZone` in Go, `.calendar(Calendar::Day, zone)` in Rust) gives a row for
  each local day, week from Monday or month in place of `every`, as
  [metrics' days](../metrics/aggregates.md#days-in-a-time-zone) do.
- `aggregate` gives a row for each span of `every` and each value of the keys
  in `by`, with its `count`. A span without events has no row, unless the
  aggregate asks for `fill`. `at` is where a row's span begins, counted from
  1970 in UTC. A row of a calendar also has `date`, the local date its span
  begins on.
- `stats` names fields, and a row gives each one's `samples`, `sum`, `min`,
  `max` and `avg` over the events whose value there is a number. `distinct`
  gives how many different values a field has.
- A sum of integers is exact, a `bigint` past 2⁵³.
- An answer is whole or an error: past 10,000 rows, or 64 MiB of different
  values held for `top` or `distinct`, it is `limit`, never a smaller answer.
  Ask for a shorter range or a wider `every`.

## Every span, and its local date

A chart wants a row for every hour, and a calendar's day by its own date.
`fill` gives a row for every span of the range, an empty one with `count: 0`,
and a calendar's row carries `date`:

```ts
const days = await api.aggregate({ names: ['call'], since: '30d', calendar: 'day', timeZone: 'Europe/Moscow', fill: true })
// [{ at, date: '2026-10-21', count: 0, by: {}, stats: {}, distinct: {} }, …, { at, date: '2026-11-19', count: 14, … }]
```

```python
days = await api.aggregate(names=["call"], since="30d", calendar="day", time_zone="Europe/Moscow", fill=True)
# [Row(at=…, count=0, …, date=datetime.date(2026, 10, 21)), …]
```

```go
days, err := api.Aggregate(ctx, records.Query{Names: []string{"call"}, Since: 30 * 24 * time.Hour,
	Calendar: tinystore.Day, TimeZone: "Europe/Moscow", Fill: true}) // days[0].Date == "2026-10-21"
```

```rust
let days = api.list().names(["call"]).since(Duration::from_hours(30 * 24))
    .aggregate(&Aggregate::new().calendar(Calendar::Day, "Europe/Moscow").fill())?; // days[0].date: Some("2026-10-21")
```

- The range is what the stream keeps of it: nothing is filled before its
  `keep`, nor past now when the query gives no `to`. The spans that cut the
  range at either end get their rows too.
- `fill` needs a start, `since` or `from`. Without one it is `invalid`.
- With `by`, every span gets a row for each value of the keys that the
  answer holds. A filled row has `count: 0`, no `stats`, and 0 for each
  `distinct`. With `by` and no events at all, there is no row to fill.
- Filled rows count toward the 10,000 rows of an aggregate, and more spans
  than that are refused before anything is read.
- `date` is `YYYY-MM-DD`: a day's own date, a week's Monday, a month's first.
  It is a `datetime.date` in Python and text elsewhere. Rows of `every` have
  none, since a span of UTC needs no time zone.
- A server of a release before `fill` refuses it as `unimplemented`.

## The largest numbers

`top('ms')` counts how often each value occurs. `largest` gives the events
themselves, those of a field's largest numbers first, such as the slowest page
opens with their `path`:

```ts
const slowest = await pages.largest('ms', { names: ['page-open'], since: '7d', limit: 20 }) // Event[]
```

```python
slowest = await pages.largest("ms", names=["page-open"], since="7d", limit=20)  # list[Event]
```

```go
slowest, err := pages.Largest(ctx, "ms", records.Query{Names: []string{"page-open"}, Since: 7 * 24 * time.Hour, Limit: 20})
```

```rust
let slowest = pages.list().names(["page-open"]).since(Duration::from_hours(7 * 24)).limit(20).largest("ms")?; // Vec<Event>
```

- The key is a field's, else a context's. An event without a number there
  is passed over.
- Events of one number come in the order `list` gives them. An integer is
  compared with a float exactly, past 2⁵³ too.
- `limit` gives how many: ten unless it says, 1,000 at most. It takes a query
  as `top` does, without `newest` and `after`, and the answer comes whole.
- It reads every event the query finds, as `top` does, and holds only the
  events it gives, out of the store's memory budget. Past the budget the
  answer is `limit`, never a shorter list.

## Sessions

What a newcomer did in their first visit: the first session of each student
who appeared since `from`, a session the events of one person each within 30
minutes of the one before, with up to 50 of its events.

```ts
const visits = await events.sessions({
	since: '30d', by: 'userId', gap: '30m', max: '4h', first: 1, events: 50,
	where: { userId: db.statement`select id from students where first_seen_at > ${from}` },
}) // Session[]: [{ by: { userId: 1 }, from, to, count: 2, events: [{ at, name, fields… }, …], partial }]
```

```python
visits = await events.sessions(
    since="30d", by="userId", gap="30m", max="4h", first=1, events=50,
    where={"userId": db.statement("select id from students where first_seen_at > ?", start)},
)  # list[Session]: Session(by={"userId": 1}, from_=…, to=…, count=2, events=[Event(…), …], partial=False)
```

```go
newcomers := db.Statement("select id from students where first_seen_at > ?", from)
visits, err := events.Sessions(ctx, records.Query{Since: 30 * 24 * time.Hour, By: []string{"userId"},
	Gap: 30 * time.Minute, Max: 4 * time.Hour, First: 1, Events: 50, Where: records.Where{"userId": newcomers}})
```

```rust
use tinystore::records::Sessions;
let newcomers = db.statement(tinystore::sql!("select id from students where first_seen_at > ?", from));
let visit = Sessions::by(["userId"]).gap(minute * 30).max(minute * 240).first(1).events(50);
let visits = events.list().since(day * 30).filter("userId", &newcomers).sessions(&visit)?; // Vec<Session>
```

| A session's  | Is                                                              |
|--------------|-----------------------------------------------------------------|
| `by`         | each key's value, null for events without it                    |
| `from`, `to` | its first event's time and its last's                           |
| `count`      | every event of it, however few it keeps                         |
| `events`     | its first `events`, as `list` gives an event; none unless asked |
| `partial`    | it may go on outside what was read, the range cutting it        |

- `by` takes keys as `aggregate` does: a field's, else a context's, or
  `stream`, `name` or `level`. A session is the events of one value of each,
  in the order of their times, those of one time as they arrived, across
  streams, packed or waiting.
- A pause longer than `gap` begins the next session. `max` ends a session that
  long after its first event, pause or none, and the next event begins a new
  one.
- `first` keeps each value's first sessions of the range; once a value has
  them, its later events are passed over as they are read.
- A session that the range's start or end cuts is answered, with `partial`:
  its first event lies within `gap` of the start, or its last within `gap` of
  the end, or of now when the range has no end.
- It reads the range once and holds one open session a value of `by`. The
  answer comes whole, in the order of the sessions' first events and then of
  their `by`, 10,000 sessions at most, open ones among them, and 64 MiB of
  what they keep: past either it is `limit`, never fewer sessions. It is not
  paged yet; a shorter range, `first` or a `where` keeps it within them.
- `events` keeps 1,000 a session at most. It takes a query as `count` does,
  without `newest`, `limit` and `after`.
- Not built yet: a funnel over sessions, and a link from a metric's sample to
  an event.

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
