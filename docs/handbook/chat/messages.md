# Messages

The chat keeps its chats and messages in an SQL database of its own, a SQLite
file in the store's directory. This chapter writes the schema as migrations,
saves messages, starts a chat and its first message in one transaction, and
reads a chat back a page at a time. It uses the SQL engine.

## The schema

A database's schema lives in migration files, one change per file, applied in
the order of their names:

```sql title="migrations/0001_chats.sql"
create table chats (
    id         integer primary key,
    user_id    integer not null,
    title      text not null,
    created_at integer not null
) strict;

create table messages (
    id         integer primary key,
    chat_id    integer not null references chats (id) on delete cascade,
    role       text not null check (role in ('user', 'assistant')),
    text       text not null,
    created_at integer not null
) strict;

create index messages_by_chat on messages (chat_id, id);
create index chats_by_user on chats (user_id, id);
```

This is plain SQLite. `strict` makes SQLite refuse a value of the wrong type,
such as text in an `integer` column, instead of storing it anyway. The
foreign key deletes a chat's messages with the chat. The `check` keeps `role`
to the two values the chat knows. Times are unix milliseconds, the way every
engine of TinyStore stores them.

## Open the database

```ts
interface Message {
	id: number
	chat_id: number
	role: 'user' | 'assistant'
	text: string
	created_at: number
}

const db = await store.database('chat', { migrations: `${import.meta.dirname}/migrations` })
const messages = db.table<Message>('messages')
```

```python
@dataclass
class Message:
    id: int
    chat_id: int
    role: str
    text: str
    created_at: int


db = await store.database("chat", migrations=Path(__file__).parent / "migrations")
messages = db.table("messages", Message)
```

```go
type Message struct {
	ID        int64
	ChatID    int64
	Role      string
	Text      string
	CreatedAt int64
}

db, err := sqldb.Open(ctx, store, "chat", sqldb.Migrations(os.DirFS("migrations")))
if err != nil {
	return err // a migration that fails, or one changed after it was applied
}
messages := sqldb.OpenTable[Message](db, "messages")
```

```rust
#[derive(Serialize, Deserialize, Debug)]
struct Message {
    id: i64,
    chat_id: i64,
    role: String,
    text: String,
    created_at: i64,
}

let db = store.database("chat").migrations("migrations").open()?;
let messages = db.table::<Message>("messages");
```

The database's file is `data/sql/chat.db`. Opening it applies the
migrations the file has not applied yet, in one transaction, and fails,
naming the file, if one that ran was edited, renamed or deleted since. A
program that starts has the schema its code expects, or it does not start:
[What opening does](../../guides/sql/migrations.md#what-opening-does) has
every case.

`db.table` is a typed handle on one table. It reads rows into your type, by
the columns' names: Go's `ChatID` is the column `chat_id`. It costs nothing
until its first call.

<div data-for="go">

In Go, `os.DirFS` reads the files from the working directory. To build them
into the binary, embed them with `//go:embed migrations/*.sql` and pass
`fs.Sub(migrations, "migrations")`.

</div>

## Save a message

```ts
const reply = await db.exec`insert into messages (chat_id, role, text, created_at)
	values (${chatId}, 'assistant', ${'Day one: Alfama.'}, ${Date.now()})`
reply.lastInsertRowid // 2: the new message's id
```

```python
reply = await db.exec(
    "insert into messages (chat_id, role, text, created_at) values (?, 'assistant', ?, ?)",
    chat_id, "Day one: Alfama.", time.time_ns() // 1_000_000,
)
reply.last_insert_rowid  # 2: the new message's id
```

```go
reply, err := db.Exec(ctx, `insert into messages (chat_id, role, text, created_at) values (?, 'assistant', ?, ?)`,
	chatID, "Day one: Alfama.", time.Now().UnixMilli())
reply.LastInsertRowid // 2: the new message's id
```

```rust
let reply = db.exec(sql!(
    "insert into messages (chat_id, role, text, created_at) values (?, 'assistant', ?, ?)",
    chat_id,
    "Day one: Alfama.",
    unix_millis(SystemTime::now())
))?;
reply.last_insert_rowid // 2: the new message's id
```

`exec` runs a write and returns after it is on the disk. It returns how many
rows changed and the id SQLite gave the inserted row. A value is always a
value, never SQL: TypeScript's `${…}` in a template, a `?` in Python and Go,
and the arguments of Rust's `sql!` all become parameters. A message that
contains `'); drop table chats; --` is stored as that text.

## Many messages at once

At a busy moment, many requests write at the same time. TinyStore puts the
writes that arrive together into one commit, so that they share one sync of
the disk, and gives each write its own savepoint inside it. If one of them
breaks a rule of the schema, only that one fails:

```scene
code ts
  await db.exec`insert into messages (chat_id, role, text, created_at)
    values (${chatId}, 'user', ${text}, ${Date.now()})`
end
actor ana    "Ana"      at 8,20  icon=user
actor ben    "Ben"      at 8,50  icon=user
actor cleo   "Cleo"     at 8,80  icon=user
actor writer "Writer"   at 50,50 icon=layers
actor disk   "chat.db"  at 90,50 icon=database
link ana writer
link ben writer
link cleo writer
link writer disk
set disk "messages: 40"

step "Three people send a message in the same millisecond. Each request calls db.exec." line=1-2
  send ana -> writer "insert"
  & send ben -> writer "insert"
  & send cleo -> writer "insert"

step "The writer takes the three into one commit, each insert in its own savepoint." line=1-2
  set writer "commit: 3 inserts"
  pulse writer

step "Ben's chat was deleted a second ago. His insert breaks the foreign key, so only his savepoint rolls back." line=1-2
  strike writer "commit: 3 inserts"
  add writer "Ben: rolled back"
  send writer -> ben "InvalidError" color=bad

step "One sync of the disk saves Ana's and Cleo's messages, and each request gets its new id." line=1-2
  send writer -> disk "1 sync"
  set disk "messages: 42"
  send writer -> ana "id 41" color=ok
  & send writer -> cleo "id 42" color=ok
```

A write in a shared commit fails alone: one broken constraint never fails its
neighbours (contract DUR-03). The error says which rule broke, so the program
can answer the user properly (contract SQL-08):

```ts
try {
	await db.exec`insert into messages (chat_id, role, text, created_at) values (${chatId}, 'user', ${text}, ${Date.now()})`
} catch (err) {
	if (err instanceof InvalidError && err.constraint?.kind === 'foreignKey') {
		return notFound('this chat is gone')
	}
	throw err
}
```

```python
try:
    await db.exec(
        "insert into messages (chat_id, role, text, created_at) values (?, 'user', ?, ?)",
        chat_id, text, time.time_ns() // 1_000_000,
    )
except tinystore.InvalidError as err:
    if err.constraint and err.constraint.kind == "foreignKey":
        return not_found("this chat is gone")
    raise
```

```go
_, err = db.Exec(ctx, `insert into messages (chat_id, role, text, created_at) values (?, 'user', ?, ?)`,
	chatID, text, time.Now().UnixMilli())
var e *tinystore.Error
if errors.As(err, &e) && e.Kind == tinystore.Invalid && e.Constraint != nil && e.Constraint.Kind == "foreignKey" {
	return notFound("this chat is gone")
}
```

```rust
let sent = db.exec(sql!(
    "insert into messages (chat_id, role, text, created_at) values (?, 'user', ?, ?)",
    chat_id,
    text,
    unix_millis(SystemTime::now())
));
if let Err(err) = &sent {
    if err.constraint().is_some_and(|c| c.kind == ConstraintKind::ForeignKey) {
        return Err(not_found("this chat is gone"));
    }
}
```

## A new chat and its first message

A chat must never exist without its first message, and the message needs the
chat's id, which only the first insert knows. That is a transaction: a
function whose writes commit together, or not at all.

```ts
const chatId = await db.tx(async tx => {
	const chat = await tx.exec`insert into chats (user_id, title, created_at)
		values (${userId}, ${'Trip to Lisbon'}, ${Date.now()})`
	await tx.exec`insert into messages (chat_id, role, text, created_at)
		values (${chat.lastInsertRowid}, 'user', ${text}, ${Date.now()})`
	return chat.lastInsertRowid
})
```

```python
async def start(tx: tinystore.SqlTx) -> int:
    chat = await tx.exec(
        "insert into chats (user_id, title, created_at) values (?, ?, ?)",
        user_id, "Trip to Lisbon", time.time_ns() // 1_000_000,
    )
    await tx.exec(
        "insert into messages (chat_id, role, text, created_at) values (?, 'user', ?, ?)",
        chat.last_insert_rowid, text, time.time_ns() // 1_000_000,
    )
    return chat.last_insert_rowid


chat_id = await db.tx(start)
```

```go
var chatID int64
err = db.Tx(ctx, func(tx *sqldb.Tx) error {
	chat, err := tx.Exec(ctx, `insert into chats (user_id, title, created_at) values (?, ?, ?)`,
		userID, "Trip to Lisbon", time.Now().UnixMilli())
	if err != nil {
		return err
	}
	chatID = chat.LastInsertRowid
	_, err = tx.Exec(ctx, `insert into messages (chat_id, role, text, created_at) values (?, 'user', ?, ?)`,
		chatID, text, time.Now().UnixMilli())
	return err
})
```

```rust
let chat_id = db.tx(|tx| -> tinystore::Result<i64> {
    let now = unix_millis(SystemTime::now());
    let chat = tx.exec(sql!("insert into chats (user_id, title, created_at) values (?, ?, ?)", user_id, "Trip to Lisbon", now))?;
    tx.exec(sql!(
        "insert into messages (chat_id, role, text, created_at) values (?, 'user', ?, ?)",
        chat.last_insert_rowid,
        text,
        now
    ))?;
    Ok(chat.last_insert_rowid)
})?;
```

If the function returns, both rows are committed. If it throws or returns an
error, neither row exists. A read inside the transaction sees the
transaction's own writes.

A transaction holds the database's writer while its function runs, so keep it
short: no calls to the model, no HTTP requests, nothing that waits on the
outside world. A transaction that runs longer than five seconds is rolled
back and fails with a `limit` error, and its function runs exactly once.

## Read a chat, a page at a time

The chat screen shows the newest 50 messages and loads older ones as the user
scrolls up. The table's query builder writes that query:

```ts
const newest = messages.where({ chat_id: chatId }).orderBy('id', 'desc')
const page = await newest.list({ limit: 50 })
const older = await newest.list({ limit: 50, after: page.next }) // page.next is undefined after the last page
```

```python
newest = messages.where(chat_id=chat_id).order_by("id", "desc")
page = await newest.list(limit=50)
older = await newest.list(limit=50, after=page.next)  # page.next is None after the last page
```

```go
newest := messages.Where(sqldb.Eq{"chat_id": chatID}).OrderBy("id", sqldb.Desc)
page, err := newest.List(ctx, sqldb.Limit(50))
older, err := newest.List(ctx, sqldb.Limit(50), sqldb.After(page.Next)) // page.Next is "" after the last page
```

```rust
let newest = messages.filter(sql::eq("chat_id", &chat_id)).order_by("id", Direction::Desc);
let page = newest.list(50, None)?;
let older = newest.list(50, page.next.as_deref())?; // page.next is None after the last page
```

`list` returns the rows and a cursor, `next`, for the page after them, so a
message that arrives while the user scrolls never shows up twice, as it
would with `offset` ([Pages](../../guides/sql/tables.md#pages)).

The builder writes the SQL a person would. `newest.limit(50).toSQL()`
(`to_sql()` in Python and Rust) shows it:

```sql
select * from "messages" where "chat_id" = ? order by "id" desc limit cast(? as integer)
```

## The list of chats

The sidebar shows the user's chats, each with its last message. With a join
and a subquery, that reads best as SQL:

```ts
const chats = await db.all<{ id: number; title: string; last: string }>`
	select c.id, c.title, m.text as last
	from chats c
	join messages m on m.id = (select max(id) from messages where chat_id = c.id)
	where c.user_id = ${userId}
	order by m.id desc
	limit 20`
```

```python
@dataclass
class ChatRow:
    id: int
    title: str
    last: str


chats = await db.all(
    ChatRow,
    """select c.id, c.title, m.text as last
    from chats c
    join messages m on m.id = (select max(id) from messages where chat_id = c.id)
    where c.user_id = ?
    order by m.id desc
    limit 20""",
    user_id,
)
```

```go
type ChatRow struct {
	ID    int64
	Title string
	Last  string
}

chats, err := sqldb.All[ChatRow](ctx, db, `
	select c.id, c.title, m.text as last
	from chats c
	join messages m on m.id = (select max(id) from messages where chat_id = c.id)
	where c.user_id = ?
	order by m.id desc
	limit 20`, userID)
```

```rust
#[derive(Deserialize, Debug)]
struct ChatRow {
    id: i64,
    title: String,
    last: String,
}

let chats: Vec<ChatRow> = db.all(sql!(
    "select c.id, c.title, m.text as last
     from chats c
     join messages m on m.id = (select max(id) from messages where chat_id = c.id)
     where c.user_id = ?
     order by m.id desc
     limit 20",
    user_id
))?;
```

`all` returns every row, `one` returns one row or nothing, and `scalar`
returns one value, such as a `count(*)`. Each of them also takes a write with
`returning`, which runs on the writer and returns its rows after the commit.
Use the builder when the query depends on the request, and SQL when it is
fixed or has joins and subqueries ([Builder or
SQL?](../../guides/sql/tables.md#builder-or-sql)).

## Change the schema

To change the schema, add a new migration file. Never edit one that has run:

```sql title="migrations/0002_pinned.sql"
alter table chats add column pinned integer not null default 0 check (pinned in (0, 1));
```

The next start applies `0002_pinned.sql` and records it. If you edit
`0001_chats.sql` after it ran, every start refuses to open the database,
until the file is back as it was.

## When a write fails

A taken key fails with a `conflict` error, a broken `check` or foreign key
with `invalid`, a transaction past five seconds with `limit`, and a shared
commit whose sync failed with `outcome unknown`. Every error names the
database and the statement or migration it is about. [When a write
fails](../../guides/sql/writes.md#when-a-write-fails) says what to do about
each.

## Why this design?

**Why SQL stays SQL.** An ORM hides the query, and then you debug a query you
cannot see. In TinyStore every call takes SQL, and the query builder writes
the SQL a person would write and shows it with `toSQL()`. TinyStore takes
care of what is around the SQL: the file, its connections, migrations, a
commit shared by many requests, and bounds so that one query cannot take the
process down.

**Why every start checks old migrations.** A migration that someone edited
after it ran means the files on disk and the code disagree about the schema,
and nothing tells you until a query fails in production. TinyStore keeps a
checksum of each applied migration and refuses to open the file when one
changed.

**Why writes share a commit but a transaction does not.** Writes known
before they run share one sync, each in a savepoint of its own, while a
transaction decides what to write from what it reads, so it holds the writer
alone and pays its own sync. [The journey of a
write](../inside/journey-of-a-write.md#why-this-design) gives the reasons.

**Why a transaction runs once and stops at five seconds.** Some libraries run
a transaction's function again when it conflicts. If that function called a
payment API, the card would be charged twice. TinyStore runs it once.
While it runs, every other write to the file waits, so a transaction that
hangs would stop the whole database. The five-second bound turns that hang
into an error.

**Why a statement may not set a pragma or begin a transaction.** All the
writes of a commit share one connection, so a `pragma foreign_keys = off` in
one write would change the rules for the writes beside it. TinyStore refuses
such a statement (contract SQL-02), and [Every connection the
same](../inside/sqlite.md#every-connection-the-same) lists what it refuses.

**Why `undefined` in a condition is an error.** `messages.where({ chat_id:
undefined })` could mean "every chat". If a request forgot to send the chat's
id, that would show one user every message of every user. TinyStore refuses
the condition instead, and refuses `update` and `delete` with an empty one
(contract SQL-09). A filter that applies only sometimes is an `if` in your
code.

## See also

- [The chat we build](../start/chat.md): where this chapter fits in the
  application.
- [One directory, many engines](../start/directory.md#two-engines-in-one-file):
  keep a bucket or a queue inside this database, so that it commits with the
  rows.
- [Limits and defaults](../production/limits.md#sql): every bound of the SQL
  engine, from a transaction's five seconds to the readers.
- [SQL API reference](../../reference/bun/sql.md): every call of the SQL engine.
