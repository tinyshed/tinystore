# records: the API book

Draft, 11 October 2026, after a newcomer check and the owner's review of its
names (below). An application's logs and events: the lines its logger writes,
an audit trail, the clicks of a page, another program's output; read back by
time and by what they hold, shown live, counted, and copied elsewhere each
once. This book is the API before the code; [dx.md](../dx.md) has the rules
it follows. TypeScript comes first in each section because most callers will
read it; Python, Go and Rust follow when they spell something differently.
The engine is `records`: its module, its file and its documents use the
word, and a program never needs it. The Go engine at `e81a050` and research's
[design/records.md][design] are what this ports: its promises are GATES.md's,
its storage was measured.

[design]: https://github.com/tinyshed/research/blob/main/tinystore/design/records.md

## What a newcomer learns

| Concept             | In one line                                                                                    |
|---------------------|------------------------------------------------------------------------------------------------|
| a logger            | prints a line as it is logged and keeps it, as `store.logger('api')` opens it                  |
| events              | a named stream of events, as `store.events('audit')` opens it; `store.events()` reads them all |
| an event            | 256 KiB; 128 keys in its fields, and as many in its context                                    |
| `fields`, `context` | what happened, given with the call; what it happened in, given once with `with`                |
| `keep`              | how long a stream's events stay: 14 days unless it says                                        |
| `tail`              | events as they are written, from now                                                           |
| `poll`              | every event once, a batch a call, from a cursor the program keeps                              |

A logger never waits and drops a line when it is written faster than the
file takes it; `append` waits and loses nothing. That is the whole choice
between `store.logger` and `store.events`: both write the same events to the
same streams, and every read finds both.

## Log

```ts
const log = store.logger('api')
log.info('signed in', { user: 42 })
log.warn('slow request', { route: '/notes', ms: 1200 })
log.error('charge failed', { order: 7, error })     // an Error is kept as its stack

const request = log.with({ requestId })              // every line of `request` carries it
request.debug('cache miss', { key })

log.event('signup', { plan: 'pro' })                 // an event named 'signup': no level, no message
```

```python
log = logging.getLogger("api")
log.addHandler(store.log_handler("api"))
log.info("signed in", extra={"user": 42})
```

```go
log := slog.New(records.Handler(store, "api"))
log.Info("signed in", "user", 42)
request := log.With("request_id", id)
```

```rust
tracing_subscriber::registry().with(store.logger("api").layer()?).init();
tracing::info!(user = 42, "signed in");
```

- A line is printed as it is logged, before anything is written, and kept
  within a second: a logger writes what it holds every second, sooner once
  half its buffer waits, at `flush()` and when the store closes.
- A call never waits and never throws. A line that finds the buffer full, or
  that the file refuses, is dropped and counted in `log.dropped`, and said on
  the console at most once in ten minutes.
- A line is an event named `log` of the logger's stream: its text is the
  `message`, what the call gave are its `fields`, what `with` gave its
  `context`.
- `log.event(name, fields)` writes an event with a name of its own, without a
  level or a message, as a line is written: at once, and dropped under load.
  An event that must not be lost is `append`'s.
- The levels are `debug`, `info`, `warn` and `error`.
- Python's logger is `logging`'s and Go's is `slog`'s, with a handler of the
  store's; in Rust it is a `tracing` layer, whose spans give the `context`,
  the trace and the span.

### What a logger is told

| Option             | Unless given                            | What it does                                                         |
|--------------------|-----------------------------------------|----------------------------------------------------------------------|
| `level`            | every level                             | the least level printed and kept                                     |
| `console`          | `'pretty'` on a terminal, else `'json'` | how a line is printed; `'off'` prints none, and the line is kept     |
| `time`             | `'clock'`                               | a pretty line's time: `'clock'`, `'full'` or `'off'`                 |
| `hideStream`       | shown                                   | leaves the stream out of a pretty line                               |
| `to`               | standard error                          | where the printed lines go                                           |
| `redact`           | nothing hidden                          | the names of secrets, whose values are hidden                        |
| `keepUrlPasswords` | hidden                                  | leaves the password of a URL inside a value as it is                 |
| `replace`          | none                                    | `(key, value) => value`, run on each field before it is hidden       |
| `source`           | none                                    | adds where the line was logged: its function, file and line          |
| `env`              | `LOG_LEVEL`, `LOG_FORMAT`, `LOG_TIME`   | `{ prefix: 'APP' }` reads `APP_LOG_LEVEL` and the rest; `false` none |
| `buffer`           | 1024 lines                              | the lines held between writes                                        |
| `keep`             | the stream's                            | how long the stream keeps its events, as `store.events` takes it     |

```ts
const log = store.logger('api', { level: 'info', console: 'json', redact: secrets, keep: '30d' })
```

- The environment wins over the code, so that a deployment changes a format
  without a build: `LOG_FORMAT=json`, `LOG_LEVEL=warn`, `LOG_TIME=full`. It
  never turns on a console the code turned off. A value none of them can mean
  is ignored, and said once.
- `redact` hides a value wherever its key names a secret, in the store and on
  the console, at any depth of a value: a key is split into words, and a name
  hides it when the name's words stand together in it. `'api key'` hides
  `apiKey`, `api_key` and `API-KEY`; `'token'` hides `bot_token` and not
  `tokens_used`. `secrets` is the usual names. A message's text is left as
  it is.
- Colours need a terminal, and neither `NO_COLOR` nor a `TERM` of `dumb`;
  `FORCE_COLOR` makes a pipe pretty and coloured, and `NO_COLOR` still wins.
- A line printed is the same bytes in every language, as the console's
  vectors keep it.

### The console alone

```ts
import { logger, secrets } from 'tinystore'

const log = logger('app', { redact: secrets })   // printed, and kept nowhere
```

A program that wants the logger and no store takes it from the package: the
same calls, the same options but `buffer` and `keep`, the same lines.
`store.logger('app')` in its place keeps them too. In TypeScript and Python
it loads nothing native. In Rust it is the crate's `console` feature, which
must link no SQLite: see the open questions.

## Events

```ts
const audit = store.events('audit', { keep: '365d' })

await audit.append('role.changed', { user: 42, role: 'admin' })
await audit.append('role.changed', { user: 42, role: 'admin' }, { at: changedAt, context: { actor: admin.id } })
await clicks.append(batch.map(click => ({ name: click.type, fields: click.props, at: click.time })))   // all or none
```

```python
audit = store.events("audit", keep="365d")
await audit.append("role.changed", {"user": 42, "role": "admin"})
await audit.append("role.changed", {"user": 42, "role": "admin"}, at=changed_at, context={"actor": admin.id})
```

```go
audit, err := records.Events(store, "audit", records.Keep(365*24*time.Hour))
err = audit.Append(ctx, "role.changed", records.Fields{"user": 42, "role": "admin"})
actor := records.Fields{"actor": admin.ID}
err = audit.Append(ctx, "role.changed", change, records.At(changedAt), records.Context(actor))
```

```rust
let audit = store.events("audit").keep(Duration::from_hours(365 * 24)).open()?;
audit.append("role.changed", &RoleChanged { user: 42, role: "admin" })?;
audit.event("role.changed").at(changed_at).context(&Actor { actor: admin.id }).append(&change)?;
```

- A stream's name is `[a-z0-9][a-z0-9_-]{0,63}`, as a bucket's. A stream
  begins with its first event; nothing declares it.
- `append` returns once the event is written, and a read finds it at once. A
  crash of the program loses none; a power loss may take the last ones, since
  the events' file is `'os'` unless the store opened `'full'`. Appends from
  many callers share a commit.
- An event's name says what happened, `role.changed`; its `fields` are any
  object JSON writes. `context` says what it happened in: who asked, which
  request. `level` and `message` may be given too, as a logger's line has
  them.
- `at` is the event's time, now unless given. It is taken from as long ago
  as the stream keeps to ten minutes ahead of the store's clock; outside it
  is `too old` or `too new`. An application taking events from clocks it does
  not own gives them the time it received them and keeps theirs as a field.
- A list is written whole or not at all, and a refused event names itself:
  its place in the list and its name.
- `append` hides nothing: `redact` is a logger's, and reads a field's key,
  never a message's text. What may hold a secret is left out by the program.
- `store.events()` without a name only reads: it has no `append` and no
  `lines`, and a call of either does not compile. Rust and Go, where a name
  has one signature, spell it `store.all_events()` and
  `records.AllEvents(store)`, and a condition `filter`, as sql's builder
  does where `where` is the language's.

### How long a stream keeps

`keep` is the stream's own, kept in the file with it: a logger and an
`events` of one name share it, and so do a program's two runs.

- An open that gives no `keep` changes nothing. A stream nobody ever gave
  one keeps 14 days.
- An open that gives one sets it. While the store is open, giving the same
  again is fine and giving another is `invalid`, naming both, so that one
  part of a program never shortens what another part keeps. To change it,
  change the code and start again.
- An event older than its stream keeps is never read, whether or not it has
  left the file yet, and never written.

## What an event carries

| Field               | Meaning                                                                              |
|---------------------|--------------------------------------------------------------------------------------|
| `at`                | its time; the file keeps nanoseconds, and events are read in its order               |
| `stream`            | the stream it was written to                                                         |
| `name`              | what happened; `log` for a logger's line                                             |
| `level`             | `trace`, `debug`, `info`, `warn`, `error` or `fatal`; absent on an event without one |
| `message`           | a line's text; absent on an event without one                                        |
| `fields`            | what the call gave                                                                   |
| `context`           | what `with`, or `append`'s `context`, gave                                           |
| `traceId`, `spanId` | 32 and 16 hex digits, as a `traceparent` header writes them; absent outside a trace  |

- In TypeScript `fields` and `context` are objects, each value as
  `JSON.parse` reads the JSON it was written as, an integer past 2⁵³ a
  `bigint`, as an SQL answer's is.
- The file keeps each value as it was spelled: `1.2300`, `-0`, the order of
  the keys and a key written twice come back byte for byte, which Rust and Go
  give as the value's JSON.
- `trace` and `fatal` are levels other programs write, which `lines` finds;
  Go keeps `slog`'s number, which may stand between two names.

## Read

```ts
const page = await store.events().list({ since: '1h', minLevel: 'warn' })               // { events, next }
const more = await store.events().list({ since: '1h', minLevel: 'warn', after: page.next })

for await (const event of store.events().all({ since: '24h', traceId })) { … }          // one request, every stream
const resets = await store.events().list({ since: '1h', search: 'connection reset' })
const mine = await audit.list({ where: { user: 42 }, newest: true, limit: 50 })         // one stream
```

```python
page = await store.events().list(since="1h", min_level="warn")
async for event in store.events().all(since="24h", trace_id=trace_id): ...
```

```go
page, err := records.AllEvents(store).List(ctx, records.Since(time.Hour), records.MinLevel(slog.LevelWarn))
for event, err := range audit.All(ctx, records.Where("user", 42), records.Newest()) { … }
```

```rust
let page = store.all_events().list().since(Duration::from_hours(1)).min_level(Level::WARN).page()?;
for event in audit.list().filter("user", 42).newest().all() { … }
```

| In a query   | Finds                                                                                                 |
|--------------|-------------------------------------------------------------------------------------------------------|
| `since`      | the span before now, `'1h'`; or `from` and `to`                                                       |
| `from`, `to` | the range's start, included, and its end, excluded; open when absent                                  |
| `streams`    | any of these streams; `store.events()`'s alone, a stream reads its own                                |
| `names`      | any of these names: `['log']` is the lines, `['click', 'view']` two kinds of event                    |
| `minLevel`   | this level and worse; an event without a level is not found                                           |
| `traceId`    | one trace's events                                                                                    |
| `where`      | each key with this value, in `fields` or in `context`                                                 |
| `search`     | this text in the message, the name or a value of `fields` or `context`, as a phrase, its case ignored |
| `newest`     | newest first; oldest first without it                                                                 |
| `limit`      | the events of a page: 1000, and 10,000 at most                                                        |
| `after`      | a page's `next`, with the rest of the query as it was                                                 |

- Every condition given holds. An answer is in the order of the events'
  times, those of one time in the order they were written.
- `list` gives one page from one moment of the file, and `all` every page in
  turn. A page never splits a time: events of one time come together.
- A page may hold fewer events than its `limit`, or none, and still have a
  `next`: a read stops at what one read may fetch rather than failing, a
  `search` over much text soonest. The last page is the one without a
  `next`.
- `where` finds a key at the top of `fields` or of `context`, by the JSON its
  value was written as: `42` finds `42`, and neither `'42'` nor `42.0`.
- `search` finds its text wherever the event holds it: in the message, in
  the name, and in a value at any depth, a string as it reads and any other
  value as its JSON was written. An error's text is found whether it was the
  message or a field. It does not look at keys, which `fieldNames` lists.
- `since` is counted once: a page asked with `after` reads the range its
  first page had.
- A condition given as `undefined` is `invalid`, never a condition left out,
  as sql's is: a trace id that was not there must not read every event.
- The time, the stream, the level, the trace and a `where` on a value that
  rarely repeats, a request's id, are found without reading what cannot
  match. `search` reads every event the rest of the query leaves.

## Watch them arrive

```ts
for await (const { events, missed } of store.events().tail({ streams: ['api'], minLevel: 'warn' })) {
	if (missed > 0) socket.send(JSON.stringify({ skipped: missed }))
	for (const event of events) socket.send(JSON.stringify(event))
}
```

- A tail gives the events written after the call that made it, a batch as
  they are written: an `append`'s at once, a logger's lines when its logger
  writes them, within a second.
- It takes a query's `streams`, `names`, `minLevel`, `traceId`, `where` and
  `search`, and no range, order or limit.
- It is live and not complete. The store holds its last megabyte of events
  for its tails; a reader slower than the writers skips what left it, and
  the batch after says how many in `missed`.
- It ends when its loop is left, and when the store closes.

## Copy them elsewhere

```ts
let cursor = await shipping.get('cursor')
for (;;) {
	const batch = await store.events().poll(cursor)   // { events, cursor, expired }
	if (batch.events.length === 0) break
	if (batch.expired > 0) log.warn('events expired before their copy', { count: batch.expired })
	await warehouse.insert(batch.events)
	cursor = batch.cursor
	await shipping.set('cursor', cursor)
}
```

- `poll` gives every event once, a batch a call, from a cursor the program
  keeps: text, and nothing to read into. Without one it starts at the oldest
  event kept.
- It returns at once: with no event past the cursor, an empty batch and the
  cursor it was given.
- A cursor is kept after its events are safe, so a crash between the two
  reads them again and loses none.
- The order is the store's and not the events': a batch at a time of one
  stream, in the order of its times. What copies events sorts them where
  they land.
- An event reaches `poll` within an hour of being written, sooner in a busy
  stream. `poll` is for a copy that must be whole; `tail` is for what must be
  now.
- `expired` counts the events their streams' `keep` removed before the
  cursor reached them: more than none means the copy has a hole.
- `limit` is 1000 events unless given. Only `store.events()` polls.

## Count

```ts
const errors = await store.events().count({ since: '1h', minLevel: 'error' })   // 42

const routes = await store.events().top('route', { since: '1h', minLevel: 'error' })
// [{ value: '/notes', count: 12 }, { value: '/login', count: 7 }, …]

const hourly = await store.events().aggregate({ since: '24h', every: '1h', by: ['level'] })
// [{ at, count: 3, by: { level: 'error' } }, { at, count: 41, by: { level: 'warn' } }, …]

const latency = await api.aggregate({ since: '24h', names: ['request'], every: '5m', stats: ['duration'] })
// [{ at, count: 310, stats: { duration: { count: 310, sum: 12772, min: 3, max: 1200, avg: 41.2 } } }, …]
```

- Each takes a query as `list` does, without `newest`, `limit` and `after`.
- `count` is how many events the query finds.
- `top` gives a key's most frequent values and how many events hold each,
  the ten most frequent unless `limit` asks for more, at most 1000; values as
  frequent as each other come in the order of their JSON. The key is a
  field's or a context's, or `stream`, `name` or `level`, which are the
  event's own before a field of that name.
- `aggregate` gives a row for each span of `every` and each value of the
  keys in `by`, with the `count` of its events. Without `every` the range is
  one span; without `by` a span is one row. `at` is where a row's span
  begins, counted from 1970 in UTC. A span without events has no row.
- `stats` names fields, and a row gives each one's `count`, `sum`, `min`,
  `max` and `avg` over the events whose value there is a number. `distinct`
  names fields, and a row gives how many different values each has. A
  field with no number in a row has no `stats` there.
- A row keeps what the program named under `by`, `stats` and `distinct`, as
  the call wrote them, so that a field called `count` or `at` meets nothing
  of the row's own.
- A sum of integers is exact, a `bigint` past 2⁵³; a value with a fraction
  makes it a float's.
- An answer is whole or an error: past 10,000 rows, or past a million
  different values held for `top` or `distinct`, it is `limit`, never a
  smaller answer. Ask for a shorter range or a wider `every`.
- Each reads every event its query finds, a page at a time, and holds only
  its answer.

## What a store holds

```ts
const streams = await store.events().streams()        // [{ name: 'api', keep: '30d', oldest, newest }, …]
const names = await api.fieldNames({ since: '1h' })   // ['ms', 'requestId', 'route', 'user']
```

- `streams` gives every stream with events, by name, with what it keeps and
  the times of its oldest and newest event.
- `fieldNames` gives the keys events hold, in `fields` and in `context`,
  sorted; `prefix` narrows them and `limit` bounds them, 1000 unless given.
  It reads no packed event, whose keys the file keeps beside them, and reads
  the events not packed yet, an hour's at most. A packed segment that reaches
  into the range gives every key it holds.
- A key's values are `top`'s, and a stream's names are `top('name')`'s.

## Another program's output

```ts
const out = store.events('worker').lines()
child.stdout.on('data', chunk => out.write(chunk))
child.on('close', () => out.end())
```

- Each line becomes an event at the time it arrived, and a writer never
  waits, as a logger never does: what does not fit its buffer is dropped and
  counted.
- The lines of a stack trace, a traceback or a JSON value printed over
  several lines make one event.
- An event named `log` keeps the line as its message, byte for byte. A JSON
  line's fields and a logfmt line's pairs become the event's `fields`, named
  `json` or `logfmt`, when they spell the line again; a query finds them
  with `where`.
- The level is found where pino, logfmt, glog, Redis, log4j, zerolog and
  Postgres write it, in colour or not.
- A line longer than one event holds is dropped whole and counted.

## Traces

```ts
await withTrace({ traceId, spanId }, () => handle(request))
```

- A line logged and an event appended inside the call, however deep and
  after any `await`, take its trace and span. An event given its own keeps
  them.
- `traceId` reads one request's events across every stream.
- In Go the trace travels in the `context.Context`, in Python in a
  `contextvars` variable, and in Rust it is the span's.

## Repair

```ts
for (const damage of await store.events().damaged()) await store.events().drop(damage)
```

A changed byte is refused as `corrupt` naming what broke, and a read over it
fails rather than answering without it. `damaged` lists what the store has
met that no longer reads, with its stream and the times it held, and `drop`
removes it, so that the reads over it work again. `drop` refuses what still
reads.

## Errors

| Error                | When                                                                                                                                                                     | What to do                                               |
|----------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------|----------------------------------------------------------|
| `invalid`            | a bad name or option; an event without a name; a value JSON cannot write; `undefined` as a condition; a `keep` other than the one the stream has while the store is open | fix the call                                             |
| `too old`, `too new` | an event's time before what its stream keeps, or more than ten minutes ahead                                                                                             | give it the time it arrived, and keep its own in a field |
| `limit`              | an event or an `append` past its bound; more events at one time than a page holds; a `count`, `top` or `aggregate` past what one read may fetch                          | send less; ask for a shorter range                       |
| `corrupt`            | a read over bytes that changed                                                                                                                                           | `damaged`, then `drop`                                   |
| `closed`             | the store closed                                                                                                                                                         | open it again                                            |

Every error names the stream and the event: `events audit: event 3
"role.changed": too old`. A logger's calls return no error: what it could
not keep is counted in `dropped`.

## Bounds

| What                         | Bound                                |
|------------------------------|--------------------------------------|
| an event                     | 256 KiB, and 128 keys in all         |
| one `append`                 | 4 MiB                                |
| an event's time              | from `keep` ago to ten minutes ahead |
| `keep`                       | 14 days unless the stream says       |
| a page                       | 1000 events, 10,000 at most          |
| a logger's buffer            | 1024 lines unless `buffer` says      |
| what a store holds for tails | the last 1 MiB of events             |
| `top`                        | 10 values, 1000 at most              |
| `aggregate`                  | 10,000 rows                          |
| what a count holds           | a million different values           |
| behind which `poll` runs     | an hour at most                      |

## What the engine chooses

Nothing here is an option: how events wait before they are packed, how they
are packed and merged, what a read may fetch before its page ends, and which
keys get a filter of their own. A program meets them only as the bounds
above. `durability` is the store's, `'os'` for this file unless it says
`'full'`.

## Dashbin's asks

| Ask                                   | Where it is                                                                         |
|---------------------------------------|-------------------------------------------------------------------------------------|
| A2: secrets hidden whatever their key | `redact`, `secrets`, a URL's password hidden unless `keepUrlPasswords`, `replace`   |
| A3: a logger's environment            | `env: { prefix }`, `env: false`, `source`                                           |
| A5: a logger that links no store      | the console alone                                                                   |
| B4: a stream's own retention          | `keep` where the stream opens, kept in the file, so a sidecar's client gives it too |
| B5: aggregates                        | `count`, `top`, `aggregate`                                                         |
| B6: the names a store holds           | `streams`, `fieldNames`, `top('name')`                                              |
| B11: a live tail                      | `tail`, whose batches say what they `missed`                                        |
| C12: an event that lasts, `until`     | not in this book: no caller has asked since the sketch                              |

## Newcomer check, 11 October

A reader who had seen nothing of TinyStore read 22 call sites without their
comments. None was read wrongly; these made the reader stop.

| What tripped a newcomer                                                         | What the book does                                                                 |
|---------------------------------------------------------------------------------|------------------------------------------------------------------------------------|
| `store.stream('audit')` read as a stream of bytes, as a file's `stream()` gives | `store.events('audit')`; `stream` stays an event's field and a query's `streams`   |
| `store.records` read as the rows of a table                                     | `store.events()` reads every stream: one word for what is written and what is read |
| `tail` beside `follow`, which `tail -f` makes one word                          | `poll` for the batches a cursor continues                                          |
| `avg: 'ms'` read as a unit of the average                                       | `stats: ['duration']`, after the second check                                      |
| `records.fields()` beside an event's `fields`                                   | `fieldNames()`                                                                     |
| `tail.missed` checked after a loop that does not end                            | a tail gives batches, each with its `missed`, after the second check               |
| whether a list given to `append` is written whole                               | said: whole or not at all                                                          |
| whether `where` looks into `context`                                            | said: in `fields` or in `context`                                                  |
| what an empty page with a `next` means                                          | said: a read stops at its bound, and the last page is the one without a `next`     |
| two openers of one stream with two `keep`s                                      | `invalid` while the store is open, as a database's options are                     |

Kept as they are: `after: page.next`, which the reader would have called a
cursor, since kv's, jobs' and blobs' pages continue so, and `poll`'s cursor is
another thing, which the program keeps; `level` on a logger beside `minLevel`
in a query, since `level` is every logger's word and `LOG_LEVEL`'s; `every`
for a span of `aggregate`, which the owner's review read as each hour, where
`per` read as a rate; `append` taking a name and fields for one event and
objects for a list.

## Second check, 11 October

A second reader read the 15 call sites the first check changed, none
wrongly, and a third compared five places blind.

| What the check found                                                                 | What the book does                                                                        |
|--------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------|
| `stats: ['duration']` taken over `avg: 'duration'`: a second field adds a key        | `stats`, and a row keeps `by`, `stats` and `distinct` apart from its own `at` and `count` |
| a tail's skips taken as batches over a count, a callback and an item of another kind | a tail gives `{ events, missed }`, as `poll` gives `{ events, cursor, expired }`          |
| `expired` guessed as events, and read nowhere in the book's loop                     | it counts events, and the loop reads it                                                   |
| a `traceId` that is `undefined` reading every event                                  | `undefined` is `invalid`, as sql's                                                        |
| a child's `exit` before its last lines                                               | the example ends on `close`                                                               |
| two `keep`s of one stream: an error taken over any silent winner                     | as written; the first winning would remove a year's audit at 30 days                      |
| `fieldNames` taken over `keys` and `fields`                                          | as written                                                                                |

Kept as they are: `poll`, which the comparison put under `pull` as sounding
live, since the reader of its loop read it rightly, and chose it for the copy
and `tail` for the page; `store.events()` without a name, which the second
reader understood and would still have given a word of its own, where the
first stopped at `stream`; `expired`, which one reader would have called
`lost`, since `keep` removed them as it was told.

## Open

- The package's `logger` beside `store.logger`: a reader could not say where
  its lines go without the book. A name of its own, `consoleLogger`, says it
  and costs a second word.
- A reader who logs with `store.logger('api')` looks for the lines on the
  logger first; the guides show `store.events('api')` beside it.
- `expired` as events asks the engine to keep how many each group it removed
  held.
- The hour `poll` may run behind is how long a quiet stream's events wait to
  be packed, as the Go engine measured its batches. A shorter wait makes
  smaller batches, which cost bytes until they merge: the engine's to measure
  before the hour changes.
- `search` in the values of fields, which the Go engine did not read for it:
  what a search then costs an event is the engine's to measure, against the
  search of messages alone.
- An event's fields typed where its stream opens,
  `store.events<{ 'role.changed': RoleChanged }>('audit')`, and a `schema`
  beside it, as a queue's.
- The console alone in Rust: `rusqlite` is no option of the crate today, so
  `features = ["console"]` links SQLite until it is.
- A `keep` changed while a server runs, which today's rule leaves to its next
  start.
- An event that lasts, C12.

## Was, in Go

| Was                                             | Now                                                              |
|-------------------------------------------------|------------------------------------------------------------------|
| `records.Open(ctx, store, opts)`, one `logs`    | `store.logger(…)`, `store.events(…)`; the engine opens with them |
| `logs.Handler(stream)`, `store.records.logger`  | `store.logger(stream)`                                           |
| `Append(ctx, Record{Stream, Name, …})`          | `events(stream).append(name, fields)`                            |
| `Record`, `LogRecord`                           | an event                                                         |
| `Attrs`                                         | `fields`                                                         |
| `Body`                                          | `message`                                                        |
| `Scan`, `All`                                   | `list`, `all`                                                    |
| `Query.Attrs`, `Query.Context`                  | `where`, which looks in both                                     |
| `Query.Search`, in the body and the name        | `search`, in the values of the fields too                        |
| `Query.Budget`                                  | the engine's; a page ends at it                                  |
| `Options.Retention`, `RetentionOf`              | `keep`, the stream's                                             |
| `Follow`, `Cursor{Segment, Row}`, `Batch`       | `poll`, a cursor of text, `{ events, cursor, expired }`          |
| `Tail`, `GapError`                              | `tail`, `{ events, missed }`                                     |
| `Aggregate(AggregateRequest{Op, Field, Width})` | `count`, `top`, `aggregate` with `every`, `by` and `stats`       |
| `Streams`, `Keys`                               | `streams`, `fieldNames`                                          |
| `Lines(stream)`                                 | `events(stream).lines()`                                         |
| a time as nanoseconds, a level as a number      | a `Date`, a level's name                                         |
| `console.Handler(stream)`, `logger(stream)`     | `logger(stream)` of the package                                  |
