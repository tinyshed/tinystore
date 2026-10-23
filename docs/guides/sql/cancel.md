# Stopping a slow query

A query that nobody waits for any more should stop. When a visitor closes a
tab or a request passes its timeout, cancel the query: TinyStore stops its
SQLite statement at once, gives its connection back to the database, and the
call fails as cancelled. The same works for the reads of metrics and records.

```ts
import { withSignal } from '@tinyshed/tinystore'

// Bun.serve: request.signal aborts when the client goes away
const signal = AbortSignal.any([request.signal, AbortSignal.timeout(2000)])
const totals = await withSignal(signal, () =>
	db.all`select user_id, sum(total) from orders group by user_id`,
)
```

```python
# a web framework cancels the handler's task when the client goes away
async with asyncio.timeout(2):
    totals = await db.all("select user_id, sum(total) from orders group by user_id")
```

```go
// net/http cancels r.Context() when the client goes away
ctx, cancel := context.WithTimeout(r.Context(), 2*time.Second)
defer cancel()
totals, err := sqldb.All[Total](ctx, db, `select user_id, sum(total) from orders group by user_id`)
```

```rust
use tinystore::CancellationToken;

let token = CancellationToken::timeout(Duration::from_secs(2));
let totals: Vec<Total> = token.run(|| db.all("select user_id, sum(total) from orders group by user_id"))?;
```

Each language uses its own way to cancel: an `AbortSignal` in Bun and Node, a
cancelled task in Python, a `context.Context` in Go, and a
`CancellationToken` in Rust.

## Stop the queries of a request that went away

A dashboard page asks a dozen queries at once. If the visitor refreshes the
page three times, two of those pages have nobody to show them to. Cancel
their queries, so that they don't hold the database's connections while the
next visitor waits.

```ts
const visit = new AbortController()
const panels = withSignal(visit.signal, () => Promise.all(queries.map(query => db.all(query))))
visit.abort() // every query still running stops, and panels rejects with CancelledError
```

```python
panels = [asyncio.create_task(db.all(query)) for query in queries]
for panel in panels:
    panel.cancel()  # each query still running stops; awaiting it raises CancelledError
```

```go
visit, leave := context.WithCancel(r.Context())
errs := make(chan error, len(queries))
for _, query := range queries {
	go func() {
		_, err := sqldb.All[sqldb.Row](visit, db, query)
		errs <- err
	}()
}
leave() // every query still running stops, and its err is context.Canceled
```

```rust
let token = CancellationToken::new();
for query in queries {
    let (db, token) = (db.clone(), token.clone());
    tokio::task::spawn_blocking(move || token.run(|| db.all::<serde_json::Value>(query)));
}
token.cancel(); // every query still running stops with ErrorKind::Cancelled
```

In Rust, a call is blocking, so a program under tokio runs it with
`spawn_blocking`. Aborting the task does not stop the thread: cancel the token
instead. `token.run` covers every read the closure makes on its thread, of any
engine.

## What a cancelled query returns

| Language  | A cancel                                     | A timeout                                  |
|-----------|----------------------------------------------|--------------------------------------------|
| Bun, Node | `CancelledError`                             | `CancelledError`                           |
| Python    | `asyncio.CancelledError`                     | `TimeoutError`, from `asyncio.timeout`     |
| Go        | an error that `errors.Is` `context.Canceled` | `context.DeadlineExceeded`                 |
| Rust      | `ErrorKind::Cancelled`                       | `ErrorKind::Cancelled`, "past its timeout" |

A Rust error names the query and the database, as every error does:
`sql app: statement "select user_id, sum(total)…": data/sql/app.db: a read
past its timeout of 2s: cancelled`.

## What stops and what does not

- A query that runs stops its statement at once, and its connection goes back
  to the database (contract STORE-09).
- A query that waits for a free connection stops waiting.
- Over a connection to a server, the server stops the query when the client
  cancels it or disconnects (contract WIRE-10).
- A cancel stops only the reads it was given. Another caller's query on the
  same connection never stops with it.
- A write does not stop. It commits or fails as it would have.
- A query inside a transaction runs to its end. The transaction's own
  five-second bound stops it.

> **A cancelled write may still commit.** In Bun, Node, Python and Go, a
> cancel returns the call at once, but a write already sent runs to its end
> on the store. Read what it wrote before you write it again.

A timeout is checked while SQLite runs the statement. A statement that SQLite
runs as one step, such as `count(*)` of a whole table, finishes that step
first, while a cancel stops it at once.

## See also

- [Writes and transactions](writes.md): `exec`, `batch`, `tx` and the
  five-second bound.
- [Time and errors](../../handbook/inside/time-and-errors.md): every error
  kind and what to do about it.
- [The wire protocol](../../wire.md): how `CANCEL` asks the server to stop.
