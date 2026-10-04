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
logger := slog.New(logs.Handler("api", console.Redact("password"))) // records/console

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

Go, Bun and Python print exactly the same bytes for the same line. You can
choose the format, hide the time or the stream, print to a file, and change
all of it with `LOG_LEVEL`, `LOG_FORMAT` and `LOG_TIME`. See
[Console output](console.md).

## Hide secrets

```ts
import { secrets } from '@tinyshed/tinystore'

const log = store.records.logger('api', { redact: secrets })
log.info('connected', { DB_PASSWORD: 'hunter2', apiKey: 'k1', tokens_used: 812 })
// DB_PASSWORD=[redacted] apiKey=[redacted] tokens_used=812
```

```python
logging.basicConfig(handlers=[store.records.handler("api", redact=tinystore.SECRETS)], level=logging.INFO)
logging.info("connected", extra={"DB_PASSWORD": "hunter2", "apiKey": "k1", "tokens_used": 812})
# DB_PASSWORD=[redacted] apiKey=[redacted] tokens_used=812
```

```go
logger := slog.New(logs.Handler("api", console.Redact(console.Secrets...)))
logger.Info("connected", "DB_PASSWORD", "hunter2", "apiKey", "k1", "tokens_used", 812)
// DB_PASSWORD=[redacted] apiKey=[redacted] tokens_used=812
```

`redact` hides the values of fields whose keys name a secret, in the console
and in the store. A redacted value is never stored. The logger splits a key
into words at `_`, `-`, `.`, spaces and capital letters, and ignores case. A
key is hidden when some of its words, in a row, spell one of the names:

| Name       | Hides                                                      | Doesn't hide  |
|------------|------------------------------------------------------------|---------------|
| `password` | `password`, `DB_PASSWORD`, `PasswordHash`, `user.password` | `passwords`   |
| `api key`  | `api_key`, `apiKey`, `APIKEY`, `x-api-key`                 | `api`         |
| `token`    | `token`, `bot_token`, `oauthToken`, `token_count`          | `tokens_used` |

The list of usual secrets is `secrets` in Bun, `tinystore.SECRETS` in Python
and `console.Secrets` in Go. It contains `password`, `passwd`, `passphrase`,
`secret`, `token`, `credential`, `credentials`, `authorization`, `cookie`,
`api key`, `private key`, `secret key`, `access key`, `signing key`,
`encryption key`, `connection string` and `dsn`. Because it contains `token`,
it also hides a counter such as `token_count`. If you log such a counter, list
your own names instead.

A password inside a URL is hidden in every value, even without `redact`:
`postgres://ann:hunter2@db/app` is printed and stored as
`postgres://ann:[redacted]@db/app`. To keep URLs as they are, set
`keepUrlPasswords: true` in Bun, `keep_url_passwords=True` in Python or
`console.KeepURLPasswords` in Go.

The message itself is not searched, so don't put secrets into messages.

## Change a field before it is stored

```ts
const log = store.records.logger('api', {
	replace: (key, value) => (key === 'email' ? maskEmail(String(value)) : value),
})
```

```python
def masked(key: str, value: object) -> object:
    return mask_email(str(value)) if key == "email" else value

handler = store.records.handler("api", replace=masked)
```

```go
logger := slog.New(logs.Handler("api", console.ReplaceAttr(func(groups []string, a slog.Attr) slog.Attr {
	if a.Key == "email" {
		return slog.String(a.Key, maskEmail(a.Value.String()))
	}
	return a
})))
```

`replace` sees every field before it is redacted, printed and stored, and
returns the value to keep. In Go, `ReplaceAttr` has the signature of slog's
own hook, so a function that you already wrote for slog works unchanged, and
returning an empty `slog.Attr` drops the field. It is not called for the time,
the level or the message.

## A console without a store

```ts
import { logger } from '@tinyshed/tinystore'

const log = logger('cli') // prints, keeps nothing
```

```python
logging.basicConfig(handlers=[tinystore.handler("cli")], level=logging.INFO)  # prints, keeps nothing
```

```go
import "github.com/tinyshed/tinystore/records/console"

slog.SetDefault(slog.New(console.Handler("cli"))) // prints, keeps nothing
```

The same logger works without a store. Use it in tools and scripts, so that
every program of your project prints its logs the same way. It takes the same
options as a store's logger. In Go, it is its own package, `records/console`,
which doesn't import SQLite, so a program that keeps nothing doesn't link the
store.

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

|                     |                                                |
|---------------------|------------------------------------------------|
| A logger's buffer   | 1,024 lines, larger with `buffer`              |
| Writes to the store | every second, or when the buffer is half full  |
| A record            | 256 KiB; 128 context fields and 128 attributes |

## See also

- [Reading](reading.md): search what you logged.
- [Traces](traces.md): attach a trace id to every line of a request.
- [records/README.md](../../records/README.md): the full contract of the
  logger.
