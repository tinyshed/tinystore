# What it is doing

A user says "my message got no answer at 14:02". To find out why, you need the
chat's logs, every line of that one request, and the events that matter for
months, such as who deleted a chat. This chapter adds a logger that prints and
keeps every line, a trace per request, an audit stream, and reads them back.
It uses the Records engine, which keeps logs and events in `records.db`.

## A logger that keeps its lines

```ts
import { secrets, withTrace } from '@tinyshed/tinystore'

const log = store.logger('chat', { redact: secrets })
const audit = store.events('audit', { keep: '365d' })
```

```python
log = store.logger("chat", redact=tinystore.secrets)
audit = store.events("audit", keep="365d")
```

```go
handler := records.NewHandler(store, "chat", console.Redact(console.Secrets...))
log := slog.New(handler)
audit, err := records.OpenEvents(store, "audit", records.Keep(365*24*time.Hour))
```

```rust
let log = store.logger("chat").layer()?;
tracing_subscriber::registry().with(log.clone()).init();
let audit = store.events("audit").keep(Duration::from_hours(365 * 24)).open()?;
```

`store.logger('chat')` is a logger whose lines go to two places: the console,
as any logger prints them, and the stream `chat` in the store. Go's is a
`slog` handler and Rust's is a `tracing` layer, so the rest of a program logs
as it always did, and libraries that log through `slog` or `tracing` land in
the store too.

`redact: secrets` hides the values of keys that name a secret, at any depth,
such as `apiKey`, `password`, `token` and `authorization`, so a line that
logs a request's headers never puts a key in the file ([Hiding
secrets](../../guides/records/logging.md#hiding-secrets)).

## Log a request

```ts
await withTrace({ traceId, spanId }, async () => {
	const request = log.with({ requestId, route: '/chats/7/messages' })
	request.info('message received', { chatId: 7, userId: 42 })
	request.warn('model slow', { ms: 8400 })
})
```

```python
with tinystore.trace(trace_id=trace_id, span_id=span_id):
    request = log.bind(request_id=request_id, route="/chats/7/messages")
    request.info("message received", chat_id=7, user_id=42)
    request.warning("model slow", ms=8400)
```

```go
ctx = records.WithTrace(ctx, traceID, spanID)
request := log.With("requestId", requestID, "route", "/chats/7/messages")
request.InfoContext(ctx, "message received", "chatId", 7, "userId", 42)
request.WarnContext(ctx, "model slow", "ms", 8400)
```

```rust
let span = tracing::info_span!("request", request_id = %request_id, route = "/chats/7/messages");
let _entered = span.enter();
tracing::info!(chat_id = 7, user_id = 42, "message received");
tracing::warn!(ms = 8400, "model slow");
```

On a terminal the lines print like this:

```log
14:02:11.204 INFO  chat  message received  chatId=7 userId=42 requestId=r-81 route=/chats/7/messages
14:02:19.611 WARN  chat  model slow  ms=8400 requestId=r-81 route=/chats/7/messages
```

`with` (`bind` in Python, a span in Rust) gives every line of the request its
id and route. The trace id is the one in the request's `traceparent` header,
so one request's lines can be found later across every stream, the worker's
included. [Traces](../../guides/records/traces.md) says how the trace travels
in each language.

A log call never waits for the disk: a full buffer drops the line and counts
it ([Never waits](../../guides/records/logging.md#never-waits)).

## Events that must stay

An audit trail is not a log: each entry must be written before the request
answers, and kept for a year.

```ts
await audit.append('chat.deleted', { chatId: 9, userId: 42 })
```

```python
await audit.append("chat.deleted", {"chat_id": 9, "user_id": 42})
```

```go
_, err = audit.Append(ctx, "chat.deleted", records.Fields{"chatId": 9, "userId": 42})
```

```rust
audit.append("chat.deleted", &Deleted { chat_id: 9, user_id: 42 })?;
```

`append` returns after the event is written, and a read finds it at once. An
event has a name that says what happened and fields that say the details. It
takes the trace of the request it was appended in, like a log line does.

Each stream keeps its events for its own `keep`, `audit` for 365 days, and
older events go with no cleanup job ([How long a stream
keeps](../../guides/records/events.md#how-long-a-stream-keeps)).

## Read them back

The user's complaint gives a time. Find the warnings around it, then the
whole request:

```ts
const page = await store.events().list({ since: '1h', minLevel: 'warn' })
const request = await store.events().list({ since: '24h', traceId })
const resets = await store.events().list({ since: '1h', search: 'connection reset' })
```

```python
page = await store.events().list(since="1h", min_level="warn")
request = await store.events().list(since="24h", trace_id=trace_id)
resets = await store.events().list(since="1h", search="connection reset")
```

```go
events, err := records.OpenAllEvents(store)
page, err := events.List(ctx, records.Query{Since: time.Hour, MinLevel: slog.LevelWarn})
request, err := events.List(ctx, records.Query{Since: 24 * time.Hour, TraceID: traceID})
resets, err := events.List(ctx, records.Query{Since: time.Hour, Search: "connection reset"})
```

```rust
let events = store.all_events()?;
let page = events.list().since(Duration::from_hours(1)).min_level(Level::WARN).page()?;
let resets = events.list().since(Duration::from_hours(1)).search("connection reset").page()?;
```

`store.events()` without a name reads every stream at once, a page of events
at a time, oldest first, and `next` reads the page after. `where: { userId:
42 }` finds the events whose fields or context hold that value, and `search`
finds a phrase anywhere in an event: [A query](../../guides/records/reading.md#a-query)
lists every condition.

How often does something go wrong, and where?

```ts
await store.events().count({ since: '1h', minLevel: 'error' }) // 42
await store.events().top('route', { since: '1h', minLevel: 'error' })
// [{ value: '/chats/8/messages', count: 31 }, { value: '/sign-in', count: 11 }]
await store.events().aggregate({ since: '24h', every: '1h', by: ['level'] })
```

```python
await store.events().count(since="1h", min_level="error")
await store.events().top("route", since="1h", min_level="error")
await store.events().aggregate(since="24h", every="1h", by=["level"])
```

```go
n, err := events.Count(ctx, records.Query{Since: time.Hour, MinLevel: slog.LevelError})
top, err := events.Top(ctx, "route", records.Query{Since: time.Hour, MinLevel: slog.LevelError})
hourly, err := events.Aggregate(ctx, records.Query{Since: 24 * time.Hour, Every: time.Hour, By: []string{"level"}})
```

```rust
let errors = events.list().since(Duration::from_hours(1)).min_level(Level::ERROR).count()?;
let routes = events.list().since(Duration::from_hours(1)).min_level(Level::ERROR).top("route")?;
```

An admin page can also follow new events as they arrive, with `tail`, and a
program that copies every event to a warehouse uses `poll` with a cursor it
keeps. And from a terminal, while the chat runs:

```sh
tinystore logs ./data -f --level warn            # every new warning, until Ctrl+C
tinystore logs ./data --since 1h --grep "reset"  # the last hour's lines that mention it
```

## Why this design?

**Why logs live in the store.** The usual stack ships logs from the program
to an agent, from the agent to a log server, and reads them in a third tool.
Each hop can drop lines, and each is a service to run. Here the logger writes
straight into `records.db`, the CLI and an AI agent read it with
`tinystore logs` and `tinystore mcp`, and a backup takes the logs with
everything else.

**Why a logger drops lines instead of waiting.** A request must never wait
for its own log line. When the disk is slow, the choice is between slowing
every request and losing some lines, and losing lines is better. TinyStore
drops them, counts them, and says so, so the loss is visible and never
silent.

**Why logs may lose their last second to a power loss.** `records.db` is
written with the operating system's [durability](../start/directory.md#a-file-per-engine)
unless the store says otherwise, which makes a line cost almost nothing to
write. An event that must survive a power loss belongs in a store opened
with `durability: 'full'`.

**Why events are kept as columns.** Most log lines share their keys and many
of their values, so a stream's events are packed into blocks of columns, and
a query skips the blocks that cannot match. [How logs and metrics
pack](../inside/how-logs-and-metrics-pack.md) explains the format.

## See also

- [Your first store](../start/first-store.md#look-inside): `tinystore logs`
  while a program runs.
- [Limits and defaults](../production/limits.md#records-and-metrics): the
  bounds of Records, and its guides' tables, such as
  [Tail](../../guides/records/tail-and-poll.md#tail) for a tail's buffer.
- [Records API reference](../../reference/bun/records.md): every call of
  loggers, events and reads.
