# Records

The Records engine keeps logs and events in `records.db` and reads them back
by time, level, trace, fields and text. A logger prints each line to the
console as any logger does, and keeps it too. Events are named facts a
program appends and keeps as long as their stream says. Both are packed into
compact blocks by column.

```ts
const log = store.logger('api', { redact: secrets })
const audit = store.events('audit', { keep: '365d' })

log.info('signed in', { userId: 42 })
await audit.append('role.changed', { userId: 42, role: 'admin' })
const page = await store.events().list({ since: '1h', minLevel: 'warn' })
```

```python
log = store.logger("api", redact=tinystore.secrets)
audit = store.events("audit", keep="365d")

log.info("signed in", user_id=42)
await audit.append("role.changed", {"user_id": 42, "role": "admin"})
page = await store.events().list(since="1h", min_level="warn")
```

```go
log := slog.New(records.NewHandler(store, "api", console.Redact(console.Secrets...)))
audit, err := records.OpenEvents(store, "audit", records.Keep(365*24*time.Hour))

log.Info("signed in", "userId", 42)
err = audit.Append(ctx, "role.changed", records.Fields{"userId": 42, "role": "admin"})
events, err := records.OpenAllEvents(store)
page, err := events.List(ctx, records.Query{Since: time.Hour, MinLevel: slog.LevelWarn})
```

```rust
let log = store.logger("api").layer()?;
tracing_subscriber::registry().with(log.clone()).init();
let audit = store.events("audit").keep(Duration::from_hours(365 * 24)).open()?;

tracing::info!(user_id = 42, "signed in");
audit.append("role.changed", &RoleChanged { user_id: 42, role: "admin".into() })?;
let page = store.all_events()?.list().since(Duration::from_hours(1)).min_level(Level::WARN).page()?;
```

## What it guarantees

- A log call never waits: lines are kept within a second, and a line that
  finds the buffer full is dropped and counted (NOT-09).
- `append` returns after the event is written, and a read finds it at once.
- An event's values come back as they were written, byte for byte.
- A stream keeps its events for its own `keep`, 14 days unless set, with no
  cleanup job.
- A damaged segment is named, listed and removable, and never stops the
  others (contract ERR-07).

## In these guides

| Page                   | Covers                                                        |
|------------------------|---------------------------------------------------------------|
| Logging                | levels, `with`, `redact`, Go's `slog` and Rust's `tracing`    |
| Console output         | pretty or JSON, `LOG_LEVEL`, `LOG_FORMAT`, a console alone    |
| Events                 | `append`, names, fields, context, times                       |
| Reading                | `list`, `all`, `where`, `search`, `count`, `top`, `aggregate` |
| Traces                 | one request's lines across streams                            |
| Tail and poll          | live events, and every event once from a cursor               |
| Other programs' output | another process's lines as events                             |
| Repair                 | `damaged` and `drop`                                          |

New to Records? [What it is doing](../../handbook/chat/what-it-is-doing.md)
in the Handbook logs a chat's requests and reads them back.

## Reference

[Bun and Node](../../reference/bun/records.md) ·
[Python](../../reference/python/records.md) ·
[Go](../../reference/go/records.md) ·
[Rust](../../reference/rust/records.md)
