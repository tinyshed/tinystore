# Logging

A store's logger prints each line to the console as any logger does, and keeps
it in `records.db` too, where a query finds it next week. A log call never
waits and never throws: under load a line is dropped and counted, never a
request slowed.

```ts
const log = store.logger('api', { redact: secrets })

log.info('signed in', { user: 42 })
log.warn('slow request', { route: '/notes', ms: 1200 })
log.error('charge failed', { order: 7, error }) // an Error is kept as its stack

const request = log.with({ requestId }) // every line of `request` carries it
request.debug('cache miss', { key })
```

```python
log = store.logger("api", redact=tinystore.secrets)

log.info("signed in", user=42)
log.warning("slow request", route="/notes", ms=1200)
log.error("charge failed", order=7, error=error)  # an exception is kept as its traceback

request = log.bind(request_id=request_id)  # every line of `request` carries it
request.debug("cache miss", key=key)
```

```go
handler := records.NewHandler(store, "api", console.Redact(console.Secrets...))
log := slog.New(handler)

log.Info("signed in", "user", 42)
log.Warn("slow request", "route", "/notes", "ms", 1200)
log.Error("charge failed", "order", 7, "error", err)

request := log.With("request_id", requestID)
request.Debug("cache miss", "key", key)
```

```rust
let log = store.logger("api").redact(SECRETS).layer()?;
tracing_subscriber::registry().with(log.clone()).init(); // a clone is the same logger

tracing::info!(user = 42, "signed in");
tracing::warn!(route = "/notes", ms = 1200, "slow request");
let span = tracing::info_span!("request", request_id = %request_id).entered(); // its lines carry it
```

```text
16:42:24.094 INFO  api  signed in  user=42
16:42:24.094 WARN  api  slow request  requestId=r-1 route=/notes ms=1200
```

## What a line is

A line is an event named `log` of the logger's stream:

| Part      | From                                                              |
|-----------|-------------------------------------------------------------------|
| `message` | the text                                                          |
| `level`   | `debug`, `info`, `warn` or `error`                                |
| `fields`  | what the call gave                                                |
| `context` | what `with` gave (`bind` in Python, `With` in Go, a span in Rust) |

So `store.events('api')` reads the logger's lines beside any events appended
to the same stream, and every read finds both. See [Reading](reading.md).

## Each language's own logger

| Language   | The store's logger is                                                                                                    |
|------------|--------------------------------------------------------------------------------------------------------------------------|
| TypeScript | `store.logger(name)`: `debug`, `info`, `warn`, `error`, `with`, `event`                                                  |
| Python     | `store.logger(name)` with `logging`'s levels and `warning`, `bind`; `store.log_handler(name)` for a program on `logging` |
| Go         | `records.NewHandler(store, name)`, a `slog.Handler`: `slog.New(handler)`                                                 |
| Rust       | `store.logger(name).layer()`, a `tracing` layer; spans give the context                                                  |

A Python program that already logs through `logging` keeps its calls:

```python
logging.getLogger("worker").addHandler(store.log_handler("worker"))  # a record's extra becomes its fields
```

Go's handler passes `testing/slogtest`, so any `slog` code logs into the
store unchanged.

## Never waits

- A line is printed as it is logged, before anything is written.
- It is kept within a second: a logger writes what it holds every second,
  sooner once half its buffer waits, at `flush()` and when the store closes.
- A line that finds the buffer full is dropped and counted in `dropped`, and
  said on the console at most once in ten minutes (NOT-09).

Logging is not where a program should wait for a disk. What must not be lost,
an audit entry, a payment, is an event that [`append`](events.md) writes and
waits for.

```ts
await log.flush() // before a short program exits
log.dropped // lines lost since the logger opened
```

```python
await log.flush()
log.dropped
```

```go
handler.Flush()
handler.Dropped()
```

```rust
log.flush();
log.dropped();
```

## Hiding secrets

`redact` hides a value wherever its key names a secret, on the console and in
the store, at any depth:

```text
{"msg":"signed in","user":42,"apiKey":"[redacted]","url":"postgres://u:[redacted]@h/db"}
```

- A key is split into words, and a name hides it when the name's words stand
  together in it: `'api key'` hides `apiKey`, `api_key` and `API-KEY`;
  `'token'` hides `bot_token` and not `tokens_used`.
- `secrets` is the usual names: passwords, tokens, keys, cookies and the
  like.
- A URL's password inside any value is hidden too, unless
  `keepUrlPasswords`.
- `replace: (key, value) => value` runs on each field before it is hidden,
  for what a name cannot catch.
- A message's text is left as it is: put secrets in fields, never in the
  message, so `redact` can find them.

## Options

| Option    | Unless given                        | Does                                                     |
|-----------|-------------------------------------|----------------------------------------------------------|
| `level`   | every level                         | the least level printed and kept                         |
| `console` | `pretty` on a terminal, else `json` | how a line is printed; `off` prints none and keeps it    |
| `redact`  | nothing hidden                      | the names of secrets                                     |
| `source`  | none                                | adds the function, file and line of the call             |
| `buffer`  | 1,024 lines                         | the lines held between writes                            |
| `keep`    | the stream's                        | how long the stream keeps its events, 14 days unless set |

How lines look on the console is [Console output](console.md).

A store's logger reads its options through the config `log` too: a change of
its level in any process sets it for every logger of the store, as
[Live settings](../config/live.md) change any setting.

## Events from a logger

```ts
log.event('signup', { plan: 'pro' }) // an event named signup: no level, no message
```

```python
log.event("signup", plan="pro")
```

```go
handler.Event("signup", "plan", "pro")
```

```rust
log.event("signup", &Signup { plan: "pro" });
```

`event` writes an event with a name of its own, as a line is written: at
once, and dropped under load. A product analytics click can be lost; an audit
entry is `append`'s.

## See also

- [Records](README.md): the engine's overview.
- [Console output](console.md): pretty and JSON, and a logger without a store.
- [What it is doing](../../handbook/chat/what-it-is-doing.md) in the Handbook.
