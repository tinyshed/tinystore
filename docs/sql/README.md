# SQL

The SQL engine gives your application its own SQLite databases. You write the
SQL: tables, joins, reports. TinyStore handles everything around it: the file
and its connections, migrations that are checked when your program starts,
writes from many requests committed together, and values that come back
exactly as you stored them. Each database is a file, `data/sql/<name>.db`.

## Open a database

```ts
const db = await store.sql('app', { migrations: './migrations' })
```

```python
db = await store.sql("app", migrations="./migrations")
```

```go
//go:embed migrations/*.sql
var migrations embed.FS

db, err := sqldb.Open(ctx, store, "app", migrations, nil) // data/sql/app.db
```

```sql title="migrations/001_notes.sql"
create table notes (
    id         integer primary key,
    author_id  integer not null,
    title      text not null,
    done       integer not null default 0,
    created_at integer not null
) strict;
create index notes_author on notes (author_id, created_at);
```

Migrations are `.sql` files, applied in the order of their names. Opening the
database applies the ones it hasn't applied yet, all in one transaction, so
the file has either the old schema or the new one. In Go, the last argument
can be a schema declared from structs, which `Open` checks the file against.
See [Schema in Go](schema.md).

## Query

```ts
type Note = { id: number; title: string; done: number }

const notes = await db.all<Note>`select id, title, done from notes where author_id = ${userId} limit 50`
const note = await db.one<Note>`select id, title, done from notes where id = ${id}` // undefined if none
const count = await db.scalar<number>`select count(*) from notes`
```

```python
@dataclass
class Note:
    id: int
    title: str
    done: bool


notes = await db.all(Note, "select id, title, done from notes where author_id = ? limit 50", user_id)
note = await db.one(Note, "select id, title, done from notes where id = ?", note_id)  # None if none
count = await db.scalar("select count(*) from notes")
```

```go
type Note struct {
	ID    int64
	Title string
	Done  bool
}

notes, err := sqldb.All[Note](ctx, db, `select id, title, done from notes where author_id = ? limit 50`, userID)
note, found, err := sqldb.One[Note](ctx, db, `select id, title, done from notes where id = ?`, id)
count, err := sqldb.Scalar[int](ctx, db, `select count(*) from notes`)
```

In Bun, the values of a template are always arguments, never SQL, so a value
can't inject SQL. In Python, use `?` placeholders, or on Python 3.14 a
template string, `t"… where id = {note_id}"`. In Go, use `?` placeholders.

`one` returns nothing for no row and fails for two rows. `scalar` reads one
value of a query that always returns a row, such as `count(*)`.

## Write

```ts
const { changes, lastId } = await db.exec`insert into notes (author_id, title, created_at)
	values (${userId}, ${title}, ${Date.now()})`
```

```python
done = await db.exec(
    "insert into notes (author_id, title, created_at) values (?, ?, ?)", user_id, title, int(time.time() * 1000)
)
done.changes, done.last_id
```

```go
result, err := db.Exec(ctx, `insert into notes (author_id, title, created_at) values (?, ?, ?)`,
	userID, title, store.Now())
```

A write returns after it is saved to disk. Writes from many requests at the
same time are committed together, with one disk sync for the whole group, so
a database handles tens of thousands of inserts per second. One failing write
doesn't affect the others in its group. See
[Writes and transactions](writes.md).

## Reads never write

Each database has one writer connection and eight reader connections. Calls
that read, `all`, `one`, `scalar` and `query`, run on readers that can't
write. If you send an `insert` to `all`, it fails at once and tells you to use
`exec`. Calls that may write, `exec` and the `exec…` forms that return rows,
run on the writer.

## Values

| Value | Stored as | Comes back as |
|---|---|---|
| text, integers, floats, bytes | SQLite's own types | the same value |
| booleans | `0` or `1` | a boolean, when the field is a boolean |
| a time (Go `time.Time`) | unix milliseconds | the same time, in UTC |
| a UUID (Go) | text, `8-4-4-4-12` | the same UUID |
| JSON (Go `sqldb.JSON[T]`) | text, checked with `json_valid` | the decoded value |
| a date (Go `sqldb.Date`) | text, `YYYY-MM-DD` | the same date |

A value that SQLite would change is rejected before anything is written: a
`NaN`, which SQLite stores as `NULL`, or a 64-bit unsigned integer that is too
large. Bun and Python return each value as SQLite stores it, and never turn
text into a time on their own.

## In this section

- [Schema in Go](schema.md): declare tables with structs, and generate
  migrations.
- [Writes and transactions](writes.md): grouped commits, batches,
  transactions and constraint errors.
- [Full-text search](search.md): search text with SQLite's FTS5.

## Limits and defaults

| | |
|---|---|
| A database name | `[a-z0-9][a-z0-9_-]{0,63}` |
| Rows `all` returns | 64 MiB of values; use `each` for more |
| Reader connections | 8 |
| Writes committed together | 1,024, or 8 MiB |

## See also

- [sqldb/README.md](../../sqldb/README.md): the full contract of the SQL
  engine.
- [design/sqldb.md](https://github.com/tinyshed/research/blob/main/tinystore/design/sqldb.md)
  in the research repository: why it works this way, with measurements.
