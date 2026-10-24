# SQL

The SQL engine gives the application SQLite databases of its own, one file
each in `sql/` of the store's directory. Migrations are applied and checked
every time a database opens, writes from many callers share a commit, and a
query builder writes the SQL a person would for the queries that depend on a
request.

```ts
const db = await store.database('chat', { migrations: `${import.meta.dirname}/migrations` })
const messages = db.table<Message>('messages')

const sent = await db.exec`insert into messages (chat_id, role, text, created_at)
	values (${chatId}, 'user', ${text}, ${Date.now()})`
const page = await messages.where({ chat_id: chatId }).orderBy('id', 'desc').list({ limit: 50 })
```

```python
db = await store.database("chat", migrations=Path(__file__).parent / "migrations")
messages = db.table("messages", Message)

sent = await db.exec(
    "insert into messages (chat_id, role, text, created_at) values (?, 'user', ?, ?)",
    chat_id, text, time.time_ns() // 1_000_000,
)
page = await messages.where(chat_id=chat_id).order_by("id", "desc").list(limit=50)
```

```go
db, err := sqldb.Open(ctx, store, "chat", sqldb.Migrations(os.DirFS("migrations")))
messages := sqldb.OpenTable[Message](db, "messages")

sent, err := db.Exec(ctx, `insert into messages (chat_id, role, text, created_at) values (?, 'user', ?, ?)`,
	chatID, text, time.Now().UnixMilli())
page, err := messages.Where(sqldb.Eq{"chat_id": chatID}).OrderBy("id", sqldb.Desc).List(ctx, sqldb.Limit(50))
```

```rust
let db = store.database("chat").migrations("migrations").open()?;
let messages = db.table::<Message>("messages");

let sent = db.exec(sql!(
    "insert into messages (chat_id, role, text, created_at) values (?, 'user', ?, ?)",
    chat_id,
    text,
    unix_millis(SystemTime::now())
))?;
let page = messages.filter(sql::eq("chat_id", &chat_id)).order_by("id", Direction::Desc).list(50, None)?;
```

## What it guarantees

- A database opens with every migration applied, or does not open: an
  applied migration that was edited, renamed or deleted names itself
  (contract SQL-03).
- A write returns after it is on the disk, and writes from many callers share
  one commit, each failing alone (contract DUR-03).
- A transaction commits all its writes or none, runs once and holds the
  writer at most five seconds (contract SQL-07).
- A broken constraint names itself: a unique key is `conflict`, anything else
  `invalid` (contract SQL-08).
- A statement may not attach a file, begin a transaction, set a pragma or
  make a temporary table, view, trigger or index (contract SQL-02).
- A query you cancel, or whose timeout passes, stops at once and gives its
  connection back (contract STORE-09).

## In these guides

| Page                                                      | Covers                                                  |
|-----------------------------------------------------------|---------------------------------------------------------|
| [Migrations](migrations.md)                               | files, their order, checksums, changing a schema        |
| [Writes and transactions](writes.md)                      | `exec`, `batch`, `tx`, errors and `outcome unknown`     |
| [Stopping a slow query](cancel.md)                        | a cancel or a timeout that stops a query at once        |
| [Tables and queries](tables.md)                           | the builder: conditions, joins, pages, updates, deletes |
| [Rows that include rows](include.md)                      | `include`: each row with the rows of a query            |
| [Buckets and queues in a database](buckets-and-queues.md) | keys and jobs that commit with rows                     |
| [FTS5 and R*Tree](fts5.md)                                | SQLite's own full-text and spatial indexes              |

New to SQL in TinyStore? [Messages](../../handbook/chat/messages.md) in the
Handbook builds a chat's tables, writes and pages step by step.

## Async Rust

This section is about Rust only. In Bun, Node and Python every call is
already awaited, and in Go a call blocks only its goroutine.

With the `async` feature, `tinystore::aio` opens the same database with
awaited calls. You open the store and the database as before, and you add
`.await` to each call that reads or writes:

```rust
use tinystore::aio::Store;
use tinystore::sql::{self, Direction};

let store = Store::open("data", Default::default())?;
let db = store.database("chat").migrations("migrations").open()?; // opening is still a plain call
let messages = db.table::<Message>("messages");

let sent = db
    .exec(sql!("insert into messages (chat_id, role, text) values (?, 'user', ?)", chat_id, text))
    .await?;
let page = messages.filter(sql::eq("chat_id", &chat_id)).order_by("id", Direction::Desc).list(50, None).await?;
```

Each call has the same name, arguments and answer as its blocking twin.
`sql!`, `sql::eq`, `Done`, `Page` and the rest come from `tinystore::sql`,
as in blocking Rust:

| Blocking                                      | Async                                                | Where it runs                                                  | If you drop the future after it was polled |
|-----------------------------------------------|------------------------------------------------------|----------------------------------------------------------------|--------------------------------------------|
| `db.all(q)?`, `db.one(q)?`, `db.scalar(q)?`   | `db.all(q).await?`, and the same for the others      | on the store's threads                                         | the query stops at once                    |
| a query's `all`, `count`, `list` and the rest | `query.list(50, None).await?`                        | on the store's threads                                         | the query stops at once                    |
| `db.exec(q)?`, `db.batch(writes)?`            | `db.exec(q).await?`, `db.batch(writes).await?`       | queued at the writer, the commit on the store's threads        | the write still commits                    |
| `table.insert(&row)?`, `query.delete()?`      | `table.insert(&row).await?`, `query.delete().await?` | as `exec`                                                      | the write still commits                    |
| `db.tx(...)?`                                 | `db.tx(move ...).await?`                             | the closure, on the store's threads, one transaction at a time | it runs to its end                         |

No call commits, syncs the disk or waits on your executor's thread. Every
future is `Send + 'static`, so you can pass it to `tokio::spawn`, and it does
nothing until it is first polled. A statement that writes, such as an
`insert … returning id` that you run with `db.all`, goes on to the writer
once TinyStore finds that it writes. From then on it commits like `exec`,
even if you drop the future (contract DUR-11).

### Stop a slow query

```rust
let total = db.scalar::<i64>("select count(*) from messages");
match tokio::time::timeout(Duration::from_secs(2), total).await {
    Ok(total) => println!("{} messages", total?),
    Err(_) => println!("too slow: the query was stopped"), // its connection is free again
}
```

When the timeout drops the future, SQLite stops the statement and the
connection goes back at once (contract STORE-09). The same happens when an
HTTP handler's client goes away and the handler's future is dropped. A
`CancellationToken` works too: a read whose future you make inside
`token.run(|| db.all(q))` stops when the token is cancelled or its timeout
passes.

### Transactions

```rust
let moved = db
    .tx(move |tx| -> tinystore::Result<u64> {
        let done = tx.exec(sql!("update messages set chat_id = ? where chat_id = ?", to_chat, from_chat))?;
        tx.exec(sql!("delete from chats where id = ?", from_chat))?;
        Ok(done.changes)
    })
    .await?;
```

The closure is the blocking one, made `move`, because it runs on one of the
store's threads after `tx` returns. It holds the database's writer, so the
calls inside it are blocking and have no `.await`. The transactions of one
database run one at a time, in the order you first await them, and they wait
for their turn without holding a thread. The five-second bound counts from
the first poll, so the time a transaction waits for its turn counts too
(contract SQL-07).

### A bucket, a queue or an index in the database

```rust
let sessions = db.bucket::<Session>("sessions").open()?;
let indexing = db.queue::<Index>("indexing").open()?;

let (joining, leaving) = (sessions.clone(), token.clone()); // the closure owns what it uses
db.tx(move |tx| -> tinystore::Result<()> {
    tx.exec(sql!("update users set name = ? where id = ?", name, user_id))?;
    tx.with(&joining).delete(&leaving)?; // blocking, as every call in a transaction
    Ok(())
})
.await?;
let bumped = sql!("update notes set version = version + 1 where id = ?", note);
db.batch([bumped.into(), indexing.adding(&Index { note })]).await?; // one commit
let session = sessions.get(&token).await?; // outside a transaction, awaited
```

A bucket, a queue or a table's search index opens from the async database
with the same builder as in blocking Rust, and its calls are awaited. It
lives in the database's file, so a bucket or a queue joins `db.tx` through
`tx.with(&handle)` and `db.batch` through `queue.adding(&job)`, and commits
with the rows or not at all (contract DUR-06). To drop a table's search
index, await `db.drop_search_index(name)`. The drop waits for its turn with
the database's transactions, and once polled it runs to its end.

`Database::from(db)` and `db.blocking()` cross between the two APIs. Both
are cheap copies of one database, never a second one.

## Reference

[Bun and Node](../../reference/bun/sql.md) ·
[Python](../../reference/python/sql.md) ·
[Go](../../reference/go/sql.md) ·
[Rust](../../reference/rust/sql.md)
