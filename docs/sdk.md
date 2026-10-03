# The SDKs' vocabulary

One product in three languages. Go embeds the engines; Bun, Node and Python
reach the same directory through `tinystore serve`, Bun and Node through one
package whose samples here say Bun. A program moving between them should
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

In Bun the type is the spelling: `Duration` is every text the rule allows, and
a rate, `'100/s'` or `'300/7d'`, is a `Rate`, so `'1hr'` or `'100/5x'` does not
compile. Text from elsewhere, an environment variable's, is cast, `env.TTL as
Duration`, and checked when the call runs, as Python checks every spelling.

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
defer store.Timer("http_request_ms").With("route", "/users").Since(time.Now())
```

```ts
await store.metrics.ingest({ name: 'cpu', kind: 'gauge', labels: { host: 'web-1' }, samples: [[new Date(), 0.42]] })
const series = await store.metrics.read({ name: 'cpu', match: { host: 'web-1' }, since: '1h' })
const failing = await store.metrics.read({ name: 'http_requests_total', since: '1h', where: { status: oneOf('500', '502'), host: prefix('api-') } })
const buckets = await store.metrics.aggregate({ name: 'http_requests_total', since: '24h', width: '1h', op: 'increase' })
const routes = await store.metrics.aggregate({ name: 'http_requests_total', since: '24h', width: '1h', op: 'rate', by: ['route'] })
store.metrics.counter('http_requests_total').with({ route: '/users' }).inc()
const user = await store.metrics.timer('http_request_ms').with({ route: '/users' }).measure(() => users.get(id))
```

```python
await store.metrics.ingest({"name": "cpu", "kind": "gauge", "labels": {"host": "web-1"}, "samples": [(now, 0.42)]})
series = await store.metrics.read(name="cpu", match={"host": "web-1"}, since="1h")
failing = await store.metrics.read(name="http_requests_total", since="1h", where={"status": one_of("500", "502"), "host": prefix("api-")})
buckets = await store.metrics.aggregate(name="http_requests_total", since="24h", width="1h", op="increase")
routes = await store.metrics.aggregate(name="http_requests_total", since="24h", width="1h", op="rate", by=["route"])
store.metrics.counter("http_requests_total").labels(route="/users").inc()
with store.metrics.timer("http_request_ms").labels(route="/users").measure():
    user = await users.get(user_id)
```

A result is `{name, kind, labels, ...}` in every language. An instrument's
labels follow its Prometheus client: Go's `With(pairs...)`, Bun's
`with({...})`, Python's `labels(**...)`. A series the store refuses is left
out from then on, and the other instruments go on being written, in all three.

A timer times each language's own way, and writes the same three series at
every flush: `<name>_count` and `<name>_sum`, counters of the durations and of
their milliseconds, and `<name>_max`, a gauge of the longest since the flush
before, left out when there was none. A range's mean is the increase of its
sum over the increase of its count.

| | Go | Bun | Python |
|---|---|---|---|
| time a block | `defer t.Since(time.Now())` | `await t.measure(fn)`, its answer and its throw | `with t.measure():`, `async with` too |
| add a duration | `t.Record(d)` | `t.record(12.5)`, milliseconds | `t.record(0.0125)`, seconds |

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
| a record's fields | `[]Field`, each `Value` its JSON | `[key, json]` pairs; `fields(record.attrs)` an object | `(key, json)` pairs; `tinystore.fields(record.attrs)` a dict |

`fields` reads each value as `JSON.parse` and `json.loads` do, the last of a
repeated key kept; the pairs keep what they may not, `1.2300` as written.

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

## Quotas

```go
ai, err := kv.OpenQuota(ctx, state, "ai", kv.Window("session", 100, 5*time.Hour), kv.Window("weekly", 300, 7*24*time.Hour))
usage, err := ai.Allow(ctx, userID) // usage.OK, usage.RetryAfter, usage.Windows["weekly"].Left
```

```ts
const ai = store.kv.quota('ai', { session: '100/5h', weekly: '300/7d' })
const { ok, retryAfter, windows } = await ai.allow(user.id) // windows.weekly.left, its name a type
```

```python
ai = store.kv.quota("ai", session="100/5h", weekly="300/7d")
usage = await ai.allow(user.id)  # usage.ok, usage.retry_after, usage.windows["weekly"].left
```

A use counts in every window or in none, in one durable write; each window
starts at a key's first use after the last ended. `get` reads the windows
without using them, `refund` gives uses back, `delete` starts every window
anew, and `of` names a branch, as a limiter's does. A window is spelled as a
limiter's rate, its name as the program calls it.

| | Go | Bun | Python |
|---|---|---|---|
| a window | `kv.Window("weekly", 300, 7*24*time.Hour)` | `weekly: '300/7d'` | `weekly="300/7d"` |
| the answer | `kv.QuotaUsage` | `QuotaUsage<'session' \| 'weekly'>` | `QuotaUsage` |
| several uses | `AllowN(ctx, key, n)`, `RefundN` | `allow(key, n)`, `refund(key, n)` | `allow(key, n)`, `refund(key, n)` |
| a window's reset | `ResetAt time.Time`, zero before it starts | `resetAt: Date \| undefined` | `reset_at: datetime \| None` |

## Once

```go
charges, err := kv.OpenOnce[Receipt](ctx, state, "charges")
receipt, err := charges.Run(ctx, requestID, func(ctx context.Context) (Receipt, error) { return charge(ctx) })
```

```ts
const charges = store.kv.once<Receipt>('charges')
const receipt = await charges.run(requestId, () => charge())
```

```python
charges = store.kv.once("charges", Receipt)
receipt = await charges.run(request_id, charge)
```

The function runs once a key and its answer is kept, a day unless `DefaultTTL`,
`defaultTtl` or `default_ttl` says; a call of the key meanwhile, from any
client, waits for it and gets its answer, and a throw or an error keeps
nothing. In Bun and Python the function runs in the client: the server hands
it the key's run, and the answer travels back to be kept. A connection lost
between the function's return and its answer's keeping is
`OutcomeUnknownError`.

## Where a job is

```go
videos, err := jobs.OpenQueue[Video](ctx, queues, "videos", jobs.MaxRunning(2))
entry, found, err := videos.Get(ctx, id)                // Waiting, 17 Ahead
for entry, err := range videos.Watch(ctx, id) { … }     // waiting 17 … 3 … running 0.4 … done
job.Progress(0.4)                                       // in the handler
```

```ts
const videos = store.jobs.queue<Video>('videos', { maxRunning: 2 })
const entry = await videos.get(id)                      // { state: 'waiting', ahead: 17, … }
for await (const s of videos.watch(id)) send(s.state, s.ahead, s.progress)
job.progress(0.4)                                       // in the handler
```

```python
videos = store.jobs.queue("videos", Video, max_running=2)
entry = await videos.get(video_id)  # JobEntry(state="waiting", ahead=17, …)
async for s in videos.watch(video_id):
    await send(s.state, s.ahead, s.progress)
job.progress(0.4)  # in the handler
```

A job is waiting, with how many jobs run ahead of it, up to 10,000; running,
with the JSON its handler last reported; failed for good; done while the
queue's keep done keeps its key; and cancelled, which only a watch yields, as
its last entry. A watch yields the job again at each change, until it ends,
and nothing for a key that names no job. A progress lives in the store
process's memory until the attempt is settled, never in the file, and the
SDKs send the latest at most ten times a second.

| | Go | Bun | Python |
|---|---|---|---|
| at most n at once | `jobs.MaxRunning(2)` | `maxRunning: 2` | `max_running=2` |
| a state | `jobs.Waiting`, `Running`, `Failed`, `Done`, `Cancelled` | `'waiting'`, `'running'`, `'failed'`, `'done'`, `'cancelled'` | the same strings |
| a progress past 4 KiB | dropped and logged | `InvalidError` | `InvalidError` |
| a running job's cancel, in its handler | the context ends, its cause `jobs.ErrCancelled` | `job.signal` aborts, its reason a `CancelledError` | the handler's task is cancelled |
| the last run | `Ran time.Time`, `Took time.Duration`, zero before one | `ran: Date`, `took` milliseconds, undefined before one | `ran: datetime`, `took` seconds, `None` before one |

## Steps of a run

```go
hits, err := jobs.Step(ctx, job, "search", func(ctx context.Context) ([]Hit, error) {
	return search(ctx, job.Value.Text)
})
```

```ts
const hits = await job.step('search', () => search(job.value.text))
```

```python
hits = await job.step("search", lambda: search(job.value.text))
```

A step runs once in its job's run: its answer is kept, as JSON, and the
attempt after a retry, a lost lease or a restart gets it back without running
the step again. A step whose attempt ends before its answer is kept runs again,
so what it does outside the store should bear doing twice. A name is the
step's within the run, which a loop numbers: `model:1`, `tool:1`, `model:2`.
A run that ends takes its steps along, and a repeat's next run starts without
them. A work loop's job and a claimed one both have it; in Go it is a function,
since a method cannot take a type of its own.

| | Go | Bun | Python |
|---|---|---|---|
| a step | `jobs.Step(ctx, job, name, fn)` | `await job.step(name, fn)` | `await job.step(name, fn)` |
| what fn is | `func(ctx) (T, error)` | a function, async or not | a function, async or not |
| an answer JSON cannot write | `ErrInvalid` | `InvalidError` | `InvalidError` |
| an answer that no longer reads | `ErrInvalid` | as `JSON.parse` reads it | as `json.loads` reads it |

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

## What fails where no call waits

An instrument's flush, a logger's write and a gauge's function run in the
background, and a failure there has no caller to return to. Each language says
it as Go's store logs its own: `background work failed` once, again when the
error changes or ten minutes on, and `background work recovered` once; a
refused instrument, `instrument refused`, once; `gauge read failed` once while
its error stays the same; and `log lines dropped`, what a full buffer dropped,
at most once in ten minutes.

| | Go | Bun | Python |
|---|---|---|---|
| where they go | `tinystore.Options.Logger`; nil discards them | stderr, a console line of stream `tinystore` | the same bytes as Bun's |
| counted besides | records' `Stats`: `DroppedFull`, `DroppedWrite` | `metrics.failures`, `log.dropped` | `metrics.failures`, `handler.dropped` |

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
- **A timer**, beside the counter and the gauge: a count, a sum of
  milliseconds and the longest since the last flush, written as three series,
  whose mean over any range is one division; timed by `defer t.Since(...)`,
  `t.measure(fn)` and `with t.measure():`. No histogram: its percentiles would
  be approximations this engine does not promise.
- **The logger as the console**: every logger writes its lines to stderr as
  they are logged, pretty or JSON, the same bytes in every language;
  `redact` hides fields by name in the store and on the console; and a logger
  of the console alone, `records.Handler`, `logger()`, `tinystore.handler()`,
  serves a program that wants the logger and not the records, so that it
  needs no pino.
- **Jobs a program can show**: `get` says where a job is and how many run
  ahead of it, `watch` follows it to its end, a handler reports its progress,
  `cancel` stops a running job's handler, and a queue's `maxRunning` bounds
  the jobs running at once across every worker. The status and the watch are
  the queue's, named by the job's key, rather than a handle of the job
  `enqueue` would answer.
- **Once**, from a second outside list on 2 October 2026: a function run once
  a key, its answer kept for the requests sent again, in every language; the
  claim lives as long as its call, so an effect outside the store carries the
  key too. Sessions from that list were left to kv's buckets.
- **Quotas** from the same list: several windows a key, counted together or
  not at all, each from the key's first use, the `AddWithin` kv had deferred.
  In Bun a window's name is a type, and a duration's or a rate's spelling
  too.

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
