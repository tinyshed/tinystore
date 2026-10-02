# The SDKs' vocabulary

One product in three languages. Go embeds the engines; Bun and Python reach the
same directory through `tinystore serve`. A program moving between them should
find the same entities, the same names and the same meanings, each written as
its own language writes it, not three products that happen to share a binary.

[server.md](server.md#sdks) has how the SDKs reach a server; [wire.md](wire.md)
has the bytes. This file is the API a program sees, and the rules that keep the
three alike.

## Rules

- **Same entities and meanings; each language's syntax.** A series, a record,
  a page, a bucket and a queue mean one thing everywhere. Go takes a struct, Bun
  an options object, Python keyword arguments; none imitates another.
- **A type is given once, where a bucket or a queue opens.** The daily calls are
  plain verbs; what the engine chooses goes into open's options, never into a
  call (`AGENTS.md`).
- **The canonical call takes a value, not a chain.** No fluent builder is the
  API: `Range{...}`, `{ ... }`, `name=..., since=...`. Sugar may follow where a
  language makes it natural, after the value form exists.
- **A name is the same name**, spelled as each language spells it: `from` and
  `to` are Go's `From`, `To`, Bun's `from`, `to`, Python's `from_`, `to` (PEP 8's
  trailing underscore for a keyword); `since`, `after`, `next`, `limit` are the
  same word in all three.
- **`scan` answers a page, `all` walks every item, `read` answers a whole
  bounded answer.** A page is its items and `next`, where the next page begins,
  passed back as `after` (Go: `Page.Next`, the next query).
- **A time range is `since`, or `from` and `to`.** `since` is a span back from
  now; `to` absent is the open end; both kinds of start at once are refused.
- **What the store keeps is what comes back.** A value is SQLite's own, a sample
  its bits, a record's field its JSON spelling; no SDK reads a time into text or
  rounds a number on the way.
- **An error is its code's class**, carrying what it names, in every language.

## Times and durations

| | Go | Bun | Python |
|---|---|---|---|
| a moment | `time.Time`; metrics `int64` unix ms | `Date`; metrics `number` ms, records `bigint` ns | `datetime`; metrics `int` ms, records `int` ns |
| a span | `time.Duration` | ms as a number, or `'1h30m'` | `timedelta`, seconds, or `"1h30m"` |

A spelled duration is each unit once, largest first: `30d`, `1h30m`, `15m`,
`1s`, `250ms`. Python's bare number is seconds, as `asyncio` and `time.sleep`
take one; Bun's is milliseconds, as `setTimeout` takes one.

## Metrics

A series is a name, a kind and labels. The store keeps the name as the label
`__name__`, Prometheus's spelling, so a label of the application's may not
begin with `__`: no API shows it.

```go
err := store.Ingest(ctx, []metrics.Batch{{
	Series:  metrics.Series{Name: "http_requests_total", Kind: metrics.Counter, Labels: metrics.Labels{"route": "/users"}},
	Samples: []metrics.Sample{{At: time.Now().UnixMilli(), Value: 1}},
}})
results, err := store.Read(ctx, metrics.Range{Name: "cpu", Match: metrics.Labels{"host": "web-1"}, Since: time.Hour})
failing, err := store.Read(ctx, metrics.Range{Name: "http_requests_total", Since: time.Hour, Where: metrics.Where{"status": metrics.OneOf("500", "502"), "host": metrics.Prefix("api-")}})
buckets, err := store.Aggregate(ctx, metrics.AggregateRequest{
	Range: metrics.Range{Name: "http_requests_total", Since: 24 * time.Hour}, Width: time.Hour, Op: metrics.AggregateIncrease,
})
routes, err := store.Aggregate(ctx, metrics.AggregateRequest{
	Range: metrics.Range{Name: "http_requests_total", Since: 24 * time.Hour}, Width: time.Hour, Op: metrics.AggregateRate,
	By: []string{"route"},
})
store.Counter("http_requests_total").With("route", "/users").Inc()
```

```ts
await store.metrics.ingest({ name: 'cpu', kind: 'gauge', labels: { host: 'web-1' }, samples: [[new Date(), 0.42]] })
const series = await store.metrics.read({ name: 'cpu', match: { host: 'web-1' }, since: '1h' })
const failing = await store.metrics.read({ name: 'http_requests_total', since: '1h', where: { status: oneOf('500', '502'), host: prefix('api-') } })
const buckets = await store.metrics.aggregate({ name: 'http_requests_total', since: '24h', width: '1h', op: 'increase' })
const routes = await store.metrics.aggregate({ name: 'http_requests_total', since: '24h', width: '1h', op: 'rate', by: ['route'] })
store.metrics.counter('http_requests_total').with({ route: '/users' }).inc()
```

```python
await store.metrics.ingest({"name": "cpu", "kind": "gauge", "labels": {"host": "web-1"}, "samples": [(now, 0.42)]})
series = await store.metrics.read(name="cpu", match={"host": "web-1"}, since="1h")
failing = await store.metrics.read(name="http_requests_total", since="1h", where={"status": one_of("500", "502"), "host": prefix("api-")})
buckets = await store.metrics.aggregate(name="http_requests_total", since="24h", width="1h", op="increase")
routes = await store.metrics.aggregate(name="http_requests_total", since="24h", width="1h", op="rate", by=["route"])
store.metrics.counter("http_requests_total").labels(route="/users").inc()
```

A result is `{name, kind, labels, ...}` in every language. An instrument's
labels follow its Prometheus client: Go's `With(pairs...)`, Bun's
`with({...})`, Python's `labels(**...)`.

## Records

```go
logger := slog.New(logs.Handler("api", records.Redact("password")))  // never waits for the file; on stderr at once
logger.InfoContext(records.WithTrace(ctx, trace, span), "charged") // a record of that trace
slog.SetDefault(slog.New(records.Handler("app")))     // the console alone, nothing kept
page, err := logs.Scan(ctx, records.Query{Since: time.Hour, MinLevel: &warn, Limit: 100})
resets, err := logs.Scan(ctx, records.Query{Since: time.Hour, Search: "connection reset"})
next, err := logs.Scan(ctx, page.Next)                // while page.More
for record, err := range logs.All(ctx, records.Query{Since: 24 * time.Hour, TraceID: trace}) { … }
```

```ts
const log = store.records.logger('api', { redact: ['password'] })   // never waits for the server; on stderr at once
await withTrace({ traceId, spanId }, async () => log.info('charged'))   // a record of that trace
log.with({ requestId }).warn('slow request', { ms: 1200 })
log.event('user.created', { userId: 42 })
const app = logger('app')                             // the console alone, nothing kept
const page = await store.records.scan({ since: '1h', minLevel: 'warn', limit: 100 })
const resets = await store.records.scan({ since: '1h', search: 'connection reset' })
const more = await store.records.scan({ since: '1h', minLevel: 'warn', limit: 100, after: page.next })
for await (const record of store.records.all({ since: '24h', traceId })) { … }
```

```python
logging.getLogger().addHandler(store.records.handler("api", redact=["password"]))  # on stderr at once, kept
with tinystore.trace(trace_id, span_id):                       # what is logged inside takes the trace
    logging.info("charged")
logging.basicConfig(handlers=[tinystore.handler("app")], level=logging.INFO)   # the console alone
page = await store.records.scan(since="1h", min_level="warn", limit=100)
resets = await store.records.scan(since="1h", search="connection reset")
more = await store.records.scan(since="1h", min_level="warn", limit=100, after=page.next)
async for record in store.records.all(since="24h", trace_id=trace):
    ...
```

Each language logs as it already does: Go through `slog`, Python through
`logging`, Bun through a logger of the stream, and another program's output,
pino's or a child's, through `lines(stream)` in all three. A page's `next` is
opaque: where the range moved, both ends absolute, so a scan `since` an hour
continues the range it began with however late the next page is asked for.

A logger is also the program's console, so that it needs no other:

| | Go | Bun | Python |
|---|---|---|---|
| kept and shown | `logs.Handler("api", ...)` | `store.records.logger('api', {...})` | `store.records.handler("api", ...)` |
| shown alone | `records.Handler("app", ...)` | `logger('app', {...})` | `tinystore.handler("app", ...)` |
| how it is shown | `records.ConsolePretty`, `ConsoleJSON`, `ConsoleOff` | `console: 'pretty' \| 'json' \| 'off'` | `console="pretty" \| "json" \| "off"` |
| on stdout | `records.Stdout` | `stdout: true` | `stdout=True` |
| hidden fields | `records.Redact("password")` | `redact: ['password']` | `redact=["password"]` |
| from a level | `records.Level(slog.LevelInfo)` | `level: 'info'` | `level=logging.INFO` |

A line goes to stderr as it is logged, before the store has it: pretty on a
terminal, one JSON object a line otherwise, so that a terminal shows what a
person reads and a container's collector gets JSON. Every language writes the
same bytes, `records/testdata/console.json`, which all three are tested
against:

```text
11:02:11.123 WARN  api  slow request  requestId=7f3a ms=1200
{"time":"2026-10-02T11:02:11.123Z","level":"WARN","stream":"api","msg":"slow request","requestId":"7f3a","ms":1200}
```

Stderr rather than stdout because a library may not write into what a program
prints: a command's answer, or a protocol spoken over stdout. Docker,
Kubernetes and systemd keep both streams. Errors are not sent apart from the
rest: two pipes reach a collector out of order, and a line's level says what
it is. `redact` hides a field by its name, the case ignored, as a key, as the
part of a dotted key after its last dot, and at any depth of an object, in the
store and on the console; a message is not searched. A child is `With`,
`with`, or `logging.getLogger("app.db")`, each language's own.

## kv, jobs, blobs and SQL

These were alike from the start, and stay so:

| | Go | Bun | Python |
|---|---|---|---|
| a bucket | `kv.OpenBucket[Session](ctx, state, "sessions")` | `store.kv.bucket<Session>('sessions')` | `store.kv.bucket("sessions", Session)` |
| a write | `Set(ctx, key, v, kv.TTL(time.Hour))` | `set(key, v, { ttl: '1h' })` | `set(key, v, ttl="1h")` |
| a queue | `jobs.OpenQueue[Reminder](ctx, queues, "reminders")` | `store.jobs.queue<Reminder>('reminders')` | `store.jobs.queue("reminders", Reminder)` |
| a job | `Enqueue(ctx, v, jobs.After(time.Hour))` | `enqueue(v, { after: '1h' })` | `enqueue(v, after="1h")` |
| a page | `Scan(ctx, Query{After: a})` → `Page{..., Next}` | `scan({ after })` → `{ items, next }` | `scan(after=a)` → `Page(items, next)` |
| every item | `All(ctx)` | `all()` | `all()` |
| SQL | `sqldb.All[Note](ctx, db, q, args...)` | ``app.all<Note>`select … ${id}` `` | `app.all(Note, "select … ?", id)`, or a `t"…"` template |

## Configs and limiters

```go
settings, err := kv.OpenConfig[Settings](ctx, state, "app", kv.Defaults(Settings{Port: 8080}), kv.FromEnv("APP", ".env"))
err = settings.Update(ctx, func(s *Settings) { s.Port = 4000 })   // kept, seen by every handle at once
for s := range settings.Watch(ctx) { server.SetPort(s.Port) }
limit, err := kv.OpenLimiter(ctx, state, "api", kv.Rate(100, time.Second), kv.Burst(20))
allowed, err := limit.Allow(ctx, userID)                          // OK, Left, RetryAfter
```

```ts
const cfg = await store.kv.config('app', { port: 8080, origins: ['localhost'] }, { prefix: 'APP' })
await cfg.update({ port: 4000 })
cfg.watch(c => server.setPort(c.port))
const limit = store.kv.limiter('api', { rate: '100/s', burst: 20 })
const { ok, left, retryAfter } = await limit.allow(userId)
```

```python
cfg = await store.kv.config("app", Settings, prefix="APP", env_file=".env")
await cfg.update({"port": 4000})
cfg.watch(lambda c: server.set_port(c.port))
limit = store.kv.limiter("api", rate="100/s", burst=20)
ok, left, retry_after = await limit.allow(user_id)
```

A config is its defaults, then a file's values, then the environment, then
what `update` kept, each over the one before; the server keeps the changed
fields and sends every change to every watcher, so `value` is read from memory.
A variable is named by the field's path in upper snake case after the prefix,
`APP_LIMITS_RPS`, by `kv/testdata/config.json`'s rule in every language.

| | Go | Bun | Python |
|---|---|---|---|
| shape and defaults | a struct `T` and `kv.Defaults` | the defaults object | a dataclass or model, its own defaults |
| a file's values | `kv.Defaults(fromYAML)` | `file: fromYAML` | `file=from_yaml` |
| the environment | `kv.FromEnv("APP", ".env")` | read unless `env: false`; Bun loads `.env` itself | read unless `env=False`; `env_file=".env"` |
| a variable of its own | `env:"DATABASE_URL"` | `env: { dbUrl: 'DATABASE_URL' }` | `env={"db_url": "DATABASE_URL"}` |
| never kept | `secret:"true"` | `secret: ['dbUrl']` | `secret=["db_url"]` |
| a check | `kv.Validate(fn)` | `schema` | `validate=fn` |
| a rate | `kv.Rate(100, time.Second)` | `rate: '100/s'` | `rate="100/s"` |
| retry after | `time.Duration` | milliseconds | seconds |

## Cancellation

Each language's own: a `context.Context` in Go, a task's cancellation in
Python, and in Bun an `AbortSignal` every call inside `withSignal(signal, fn)`
runs under, carried as AsyncLocalStorage carries a trace; `work`, and a blob's
`put` and `get`, also take a signal of their own. All end in one `CANCEL` on
the wire.

```ts
await withSignal(AbortSignal.timeout(2000), async () => {
	const notes = await app.all`select * from notes`   // rejects once two seconds pass
})
```

## The proposals, decided

An outside list of twenty ideas was read against these rules on 2 October 2026.

Built on the branch that brought this file:

- a metric's name as a field, labels apart, `__` refused;
- `since` on every time range, metrics and records, in Go and both SDKs;
- `scan` and `all` for records as for the other engines, one page shape, an
  opaque `next` passed back as `after`, and `Page` in Python as in Bun;
- a Bun logger of a stream, never waiting, as Go's and Python's handlers are;
- spelled durations in Python as in Bun;
- every value as SQLite keeps it (the driver no longer reads a time into text).

Built after it:

- **Conditions beyond equality**, `where` beside `match`: `OneOf`, `NoneOf`
  and `Prefix` in Go, `oneOf`, `noneOf` and `prefix` in Bun, `one_of`,
  `none_of` and `prefix` in Python, each an object of the SDK's own so that a
  stored value is never read as a query; a plain value in an SDK's `where` is
  equality. `NoneOf` was `not` in the proposal: it takes several values, and
  `not` is a word Python keeps for itself.
- **Across series**: `by` and `without` on aggregates, one result a group,
  `by: []` every series of a name; and `avg`, `rate` for counters, `delta` for
  gauges, each exact and rounded once, a group's too (`aggregate-contract.md`).
- **A limit says which**: `LimitError` names the bound, what the call would
  have taken of it and the bound, `limit`, `wanted` and `bound` in both SDKs,
  `Name`, `Wanted` and `Bound` in Go.
- **Trace correlation**: a line logged, and a record appended without a trace
  of its own, take the trace the caller runs in: `records.WithTrace(ctx, …)`
  in Go, `withTrace({ traceId, spanId }, fn)` in Bun over AsyncLocalStorage,
  `with tinystore.trace(trace_id, span_id):` in Python over contextvars. No
  dependency on OpenTelemetry, whose ids are the same bytes.
- **`explain`** for metrics: what a read or an aggregate would spend of its
  limits, and the `LimitError` it would stop at, found without a payload
  fetched or a sample decoded: `ExplainRead`/`ExplainAggregate` in Go,
  `metrics.explain(range)` in both SDKs, an aggregate's when it names an op.
- **`status()`** in both SDKs: the server's version, the protocol the
  connection speaks, its engines and the connection's capability, as its
  `WELCOME` said them.
- **Cancellation on every Bun call**: `withSignal(signal, fn)`.
- **Configs and a limiter**, in kv: a config of defaults, a file, the
  environment and kept changes, hot in every process watching it; a GCRA
  limiter of requests by key. `Watch` of a bucket of values, `AddWithin` for
  quotas and history wait.
- **Text in records**: `search` finds a record whose body or name holds the
  text, the case ignored, through `scan` and `all`; its budget ends a page
  early rather than failing. An index of words waits for a measurement.
- **The logger as the console**: every logger writes its lines to stderr as
  they are logged, pretty or JSON, the same bytes in every language;
  `redact` hides fields by name in the store and on the console; and a logger
  of the console alone, `records.Handler`, `logger()`, `tinystore.handler()`,
  serves a program that wants the logger and not the records, so that it
  needs no pino.

Designed, waiting for engine work (each needs the engine, the wire and both
SDKs in one change):

- **`explain` for records**: what a records scan would open, fetch and decode.
- **`capabilities`**: what a server serves beyond its version, asked before a
  call. A client newer than its server is told already: a field or a method
  the server does not know is `UnimplementedError`, naming the field and the
  server's version (`server.md` "Versions").

Declined:

- an `observe()` scope that stamps labels on metrics and records at once:
  `counter(...).with(...)` and `logger(...).with(...)` say it where it is used;
- `kv.json`: a bucket's type already chooses JSON;
- retention changed at run time: it is an engine's choice, made where it opens;
- a fluent builder as the canonical call, and a text query language in the
  SDKs: a search box may parse one into the same conditions later;
- `tail`: a follower sees what is sealed, up to an hour behind with the default
  `SealAge`, which a name promising "now" would hide.
