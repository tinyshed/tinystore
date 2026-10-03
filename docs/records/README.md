# Records

The records engine stores your application's logs and events. Your logger
prints each line to the console and also keeps it, so you can search your logs
later by time, level, trace, fields and text. Events, such as a click or a
sign-up, are stored the same way. Records keeps its data in
`data/records.db`, compressed to a few bytes per record.

## Log and read back

```ts
const log = store.records.logger('api')

log.info('server started', { port: 3000 })
log.with({ requestId }).warn('slow request', { ms: 1200 })

const page = await store.records.scan({ since: '1h', minLevel: 'warn' })
```

```python
logging.basicConfig(handlers=[store.records.handler("api")], level=logging.INFO)

logging.info("server started", extra={"port": 3000})
logging.warning("slow request", extra={"request_id": request_id, "ms": 1200})

page = await store.records.scan(since="1h", min_level="warn")
```

```go
logs, err := records.Open(ctx, store, records.Options{}) // data/records.db
logger := slog.New(logs.Handler("api"))

logger.Info("server started", "port", 3000)
logger.With("requestId", requestID).Warn("slow request", "ms", 1200)

warn := slog.LevelWarn
page, err := logs.Scan(ctx, records.Query{Since: time.Hour, MinLevel: &warn})
```

Each language logs the way it already does: Go through `log/slog`, Python
through `logging`, and Bun through a logger of the store. On a terminal, the
lines look like this:

```text
11:02:11.123 INFO  api  server started  port=3000
11:02:14.502 WARN  api  slow request  requestId=7f3a ms=1200
```

## One model for logs and events

Every record has the same fields:

| Field | Example |
|---|---|
| time | when it happened, to the nanosecond |
| stream | the part of your system it came from: `api`, `web`, `worker` |
| name | `log` for a log line, or the event's name, such as `user.created` |
| level | debug, info, warn or error, if it has one |
| body | the message of a log line |
| trace and span ids | the request it belongs to |
| context | who produced it: a request id, a session, a host |
| attributes | what happened: a duration, a route, an element |

A value keeps its exact JSON spelling. `1.2300`, `-0` and a large integer come
back exactly as they were written, in the original order of the fields.

## In this section

- [Logging](logging.md): the logger as your console, levels, and hiding
  secrets.
- [Events](events.md): store clicks, sign-ups and other events.
- [Reading](reading.md): filter by time, level, fields and text, a page at a
  time.
- [Traces](traces.md): find every record of one request.
- [Other programs' output](lines.md): store the logs of a child process.
- [Following](following.md): process new records in order, from a cursor.

## Limits and defaults

| | |
|---|---|
| Retention | 14 days |
| A record | 256 KiB; 128 context fields and 128 attributes |
| A page of `scan` | 1,000 records, at most 10,000 |
| A logger's buffer | 1,024 lines |
| A record's time | from 14 days ago to 10 minutes ahead of the store's clock |

## See also

- [records/README.md](../../records/README.md): the full contract of the
  records engine.
- [design/records.md](https://github.com/tinyshed/research/blob/main/tinystore/design/records.md)
  in the research repository: the storage format and its measurements.
