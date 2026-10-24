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

| Page                                        | Covers                                                        |
|---------------------------------------------|---------------------------------------------------------------|
| [Logging](logging.md)                       | levels, `with`, `redact`, Go's `slog` and Rust's `tracing`    |
| [Console output](console.md)                | pretty or JSON, `LOG_LEVEL`, `LOG_FORMAT`, a console alone    |
| [Events](events.md)                         | `append`, names, fields, context, times                       |
| [Reading](reading.md)                       | `list`, `all`, `where`, `search`, `count`, `top`, `aggregate` |
| [Traces](traces.md)                         | one request's lines across streams                            |
| [Tail and poll](tail-and-poll.md)           | live events, and every event once from a cursor               |
| [Other programs' output](other-programs.md) | another process's lines as events                             |
| [Repair](repair.md)                         | `damaged` and `drop`                                          |

New to Records? [What it is doing](../../handbook/chat/what-it-is-doing.md)
in the Handbook logs a chat's requests and reads them back.

## Async Rust

With the `async` feature, `tinystore::aio::Store` opens the same streams,
and each call that reads or writes returns a future. The calls have the same
names as in the blocking API. This section shows Rust only, because in Bun,
Node and Python every call is already awaited, and in Go a call blocks its
goroutine, which costs little.

```rust title="blocking.rs"
use tinystore::Store;

let store = Store::open("data", Default::default())?;
let audit = store.events("audit").keep(Duration::from_hours(365 * 24)).open()?;
audit.append("role.changed", &RoleChanged { user_id: 42, role: "admin".into() })?;
let errors = store.all_events()?.list().since(Duration::from_hours(1)).min_level(Level::ERROR).count()?;
for event in audit.list().since(Duration::from_hours(24)).search("admin").all() {
    show(event?);
}
```

```rust title="async.rs"
use futures::StreamExt; // or tokio_stream::StreamExt: `next` on a stream
use tinystore::aio::Store;

let store = Store::open("data", Default::default())?;
let audit = store.events("audit").keep(Duration::from_hours(365 * 24)).open()?;
audit.append("role.changed", &RoleChanged { user_id: 42, role: "admin".into() }).await?;
let errors = store.all_events()?.list().since(Duration::from_hours(1)).min_level(Level::ERROR).count().await?;
let mut found = audit.list().since(Duration::from_hours(24)).search("admin").all();
while let Some(event) = found.next().await {
    show(event?);
}
```

Opening the store and a stream stays a plain call that you make once at
start, because the first open may migrate `records.db`. `append` returns
after the event is written, as in the blocking API. Every read runs on the
store's own threads, so your executor's thread never waits for the disk.

| Blocking                                           | Async                                      | Where it runs                                           | If you drop the future after its first poll |
|----------------------------------------------------|--------------------------------------------|---------------------------------------------------------|---------------------------------------------|
| `append`, `append_all`, `event(...).append`        | the same, awaited                          | checked in the poll, committed on the store's threads   | the events are still written                |
| `page`, `count`, `top`, `aggregate`, `field_names` | the same, awaited                          | on the store's threads                                  | the read stops, and its reader goes back    |
| `all()`, an `Iterator`                             | `all()`, a `Stream`                        | a page at a time on the store's threads                 | the page being read stops                   |
| `tail()` with `next_within`                        | `tail()`, a `Stream`                       | waits on no thread, each batch taken from memory        | the tail closes                             |
| `poll`, `poll_limit`, `streams`, `damaged`         | the same, awaited                          | on the store's threads                                  | the read stops                              |
| `drop(&damage)`                                    | the same, awaited                          | on the store's threads                                  | it runs to its end                          |
| `Lines`, a `std::io::Write`                        | `write`, a plain call, and `flush().await` | `write` into the buffer, `flush` on the store's threads | the flush runs to its end                   |
| `logger(...)`, `records::maintain`                 | the same, blocking                         | the logger's buffer; maintenance on the caller's thread | not a future                                |

An append's events are checked in the poll that starts it, at most 4 MiB
per append. An append of up to 256 KiB is also compressed in that poll. A
larger one, or one that arrives while another append is compressing, is
compressed on the store's threads, so the poll never waits. If the store has
a memory budget and no room for the events, the append waits for memory
without holding a thread. It fails with a limit error (`ErrorKind::Limit`)
after ten seconds, as the blocking call does.

A tail waits on no thread: each append wakes it, and its stream ends when the
store closes. To wait with a bound, use your runtime's timeout. The tail
stays open after a timeout:

```rust
let mut tail = store.all_events()?.list().streams(["api"]).min_level(Level::WARN).tail()?;
while let Ok(Some(batch)) = tokio::time::timeout(Duration::from_secs(30), tail.next()).await {
    let batch = batch?;
    if batch.missed > 0 {
        socket.send(skipped(batch.missed)).await?;
    }
    for event in &batch.events {
        socket.send(json(event)).await?;
    }
}
```

A writer of another program's lines is no `std::io::Write` in the async API,
because its `flush` would block your executor's thread. `write` never waits,
and you await `flush`:

```rust
use tokio::io::AsyncReadExt;

let mut out = store.events("worker").open()?.lines()?;
let mut stdout = child.stdout.take().unwrap();
let mut piece = vec![0; 64 << 10];
loop {
    let read = stdout.read(&mut piece).await?;
    if read == 0 {
        break;
    }
    out.write(&piece[..read])?; // never waits
}
out.flush().await?; // the lines ended so far are written
```

The logger stays as it is: a line goes to a buffer and never waits. A store
opened without background work runs `tinystore::records::maintain` on
`store.blocking()`. [Rust with async](../../handbook/start/rust-async.md) has
the rest of the async API: dropping a future, timeouts and closing.

## Reference

[Bun and Node](../../reference/bun/records.md) ·
[Python](../../reference/python/records.md) ·
[Go](../../reference/go/records.md) ·
[Rust](../../reference/rust/records.md)
