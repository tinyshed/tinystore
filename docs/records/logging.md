# Logging

The records logger is your application's console and its log store at once.
Each line is printed when you log it, formatted for people on a terminal and
as JSON everywhere else, and saved in the store in the background. Logging
never waits for the disk, so you can use it on every request instead of pino,
zap or a log shipper.

## Get a logger

```ts
const log = store.records.logger('api', { redact: ['password'] })

log.debug('cache miss', { key })
log.info('user signed in', { userId: 42 })
log.warn('slow request', { ms: 1200 })
log.error('payment failed', { orderId, reason: err.message })
```

```python
logging.basicConfig(handlers=[store.records.handler("api", redact=["password"])], level=logging.INFO)

logging.debug("cache miss", extra={"key": key})
logging.info("user signed in", extra={"user_id": 42})
logging.warning("slow request", extra={"ms": 1200})
logging.exception("payment failed", extra={"order_id": order_id})
```

```go
logs, err := records.Open(ctx, store, records.Options{})
logger := slog.New(logs.Handler("api", records.Redact("password")))

logger.Debug("cache miss", "key", key)
logger.Info("user signed in", "userId", 42)
logger.Warn("slow request", "ms", 1200)
logger.Error("payment failed", "orderId", orderID, "err", err)
```

The stream name, here `api`, says where the lines come from. Use one stream
per service or per kind of work, not per user or per session.

## Add context to every line

```ts
const requestLog = log.with({ requestId, userId })
requestLog.info('order created', { orderId }) // carries requestId and userId too
```

```python
log = logging.getLogger("api")

with tinystore.context(request_id=request_id, user_id=user_id):
    log.info("order created", extra={"order_id": order_id})  # carries request_id and user_id too
```

```go
requestLog := logger.With("requestId", requestID, "userId", userID)
requestLog.Info("order created", "orderId", orderID) // carries requestId and userId too
```

In Bun and Go, `with` creates a child logger. Python's loggers are shared by
the whole module, so Python works differently. `tinystore.context` adds the
fields to every line that is logged inside its `with` block. Lines from tasks
that the block starts get the fields too. Put it in your request middleware
once, and every line of the request carries the request's id.

The fields of `with` and `tinystore.context` are stored as the record's
context. The fields of the call itself, `extra` in Python, are stored as its
attributes. You can search by either.

## What the console shows

On a terminal, lines are colored and formatted for people:

```log
11:02:14.502 WARN  api  slow request  requestId=7f3a ms=1200
```

When stderr is not a terminal, for example in a container, each line is one
JSON object, which log collectors expect:

```text
{"time":"2026-10-02T11:02:14.502Z","level":"WARN","stream":"api","msg":"slow request","requestId":"7f3a","ms":1200}
```

Go, Bun and Python print exactly the same bytes for the same line. Colors are
off when `NO_COLOR` is set or `TERM` is `dumb`.

### Colors in an IDE

The run console of an IDE, such as GoLand, WebStorm or PyCharm, reads your
program through a pipe, not a terminal. So by default the logger prints JSON
without colors there. Set `FORCE_COLOR=1` in the run configuration's
environment variables, and the logger prints pretty lines in color:

```sh
FORCE_COLOR=1 bun app.ts
```

`FORCE_COLOR` turns colors on wherever the program runs, so set it where you
run the program, not in production. `NO_COLOR` still turns colors off.

| Option | Go | Bun | Python |
|---|---|---|---|
| format | `records.ConsolePretty`, `ConsoleJSON`, `ConsoleOff` | `console: 'pretty' \| 'json' \| 'off'` | `console="pretty" \| "json" \| "off"` |
| print to stdout instead of stderr | `records.Stdout` | `stdout: true` | `stdout=True` |
| lowest level kept | `records.Level(slog.LevelInfo)` | `level: 'info'` | `level=logging.INFO` |
| hidden fields | `records.Redact("password")` | `redact: ['password']` | `redact=["password"]` |

Lines go to stderr by default, because a library shouldn't write into what a
program prints to stdout, such as a command's output.

## Hide secrets

`redact` hides the values of fields with the given names, in the console and
in the store. A redacted value is never stored. The names are matched without
case, at any depth of an object, and also as the last part of a dotted key,
so `redact: ['password']` hides `password`, `Password` and `user.password`.
The message itself is not searched, so don't put secrets into messages.

## A console without a store

```ts
import { logger } from '@tinyshed/tinystore'

const log = logger('cli') // prints, keeps nothing
```

```python
logging.basicConfig(handlers=[tinystore.handler("cli")], level=logging.INFO)  # prints, keeps nothing
```

```go
slog.SetDefault(slog.New(records.Handler("cli"))) // prints, keeps nothing
```

The same logger works without a store. Use it in tools and scripts, so that
every program of your project prints its logs the same way.

## When the buffer is full

The logger keeps up to 1,024 lines in memory and writes them to the store
every second, or sooner when half of the buffer is full. If lines arrive
faster than they can be written, the logger drops the newest lines and counts
them, instead of slowing down your program. The count is printed once in ten
minutes at most. The console still shows every line.

A logger is for lines that a burst may lose. When every record must be kept,
such as an audit trail, use [`append`](events.md), which returns after the
records are saved.

## Limits and defaults

| | |
|---|---|
| A logger's buffer | 1,024 lines, larger with `buffer` |
| Writes to the store | every second, or when the buffer is half full |
| A record | 256 KiB; 128 context fields and 128 attributes |

## See also

- [Reading](reading.md): search what you logged.
- [Traces](traces.md): attach a trace id to every line of a request.
- [records/README.md](../../records/README.md): the full contract of the
  logger.
