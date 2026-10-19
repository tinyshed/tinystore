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
- A statement may not attach a file, begin a transaction or set a pragma
  (contract SQL-02).

## In these guides

| Page                             | Covers                                                  |
|----------------------------------|---------------------------------------------------------|
| Migrations                       | files, their order, checksums, changing a schema        |
| Writes and transactions          | `exec`, `batch`, `tx`, errors and `outcome unknown`     |
| Tables and queries               | the builder: conditions, joins, pages, updates, deletes |
| Rows that include rows           | `include`: each row with the rows of a query            |
| Buckets and queues in a database | keys and jobs that commit with rows                     |
| FTS5 and R*Tree                  | SQLite's own full-text and spatial indexes              |

New to SQL in TinyStore? [Messages](../../handbook/chat/messages.md) in the
Handbook builds a chat's tables, writes and pages step by step.

## Reference

[Bun and Node](../../reference/bun/sql.md) ·
[Python](../../reference/python/sql.md) ·
[Go](../../reference/go/sql.md) ·
[Rust](../../reference/rust/sql.md)
