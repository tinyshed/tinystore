# sqldb: the API book

Accepted by the owner, 10 October 2026, after a newcomer check and two
reviews (below). The
application's own relational data: its tables, joins, reports and
transactions, in SQLite files it owns. This book is the API before the code;
[dx.md](../dx.md) has the rules it follows, and [kv.md](kv.md) and
[jobs.md](jobs.md) the words all three share. TypeScript comes first in each
section; Python, Go and Rust follow where they spell something differently.
Nothing of it is built in Rust yet: the Go engine at `e81a050` and research's
design of it are the reference for what it promises.

What changed from the Go engine: queries can be built without writing their
text by hand, in every language; `batch` takes a list of statements and
`view` folds into `tx`; a read verb takes a write that returns rows; a
transaction exists over the wire too; and the database opens from the store
as every other handle does.

## What a newcomer learns

| Concept                        | In one line                                                                               |
|--------------------------------|-------------------------------------------------------------------------------------------|
| database                       | an SQLite file of the application's own, `sql/<name>.db`, opened with its migrations      |
| migrations                     | `.sql` files, each applied once in the order of their names, checked at every open        |
| `all`, `one`, `scalar`, `each` | the rows, one row or none, one value, the rows one at a time: what a statement gives back |
| `exec`                         | a write, durable when it returns; writes from many callers share a commit                 |
| table                          | a typed handle on one table: insert a row, and query it without writing SQL text          |
| `where`, `sql.or`              | a query's conditions: equal values, or a piece of SQL with its values                     |
| `batch`, `tx`                  | writes known before they run, as one; a read and the writes it decides: all or none       |
| `db.bucket`, `db.queue`        | kv and jobs inside the database's file, committing with its rows                          |

SQL stays SQL: every call takes it as text, and the query builder writes the
same SQL a person would, which `toSQL()` shows. TinyStore does the work around
it: the file and its connections, migrations, grouped commits, values that
come back as they went in, and bounds that keep a query from taking the
process down.

## Open a database

```ts
const db = await store.database('app', { migrations: `${import.meta.dir}/migrations` })   // data/sql/app.db
```

```python
db = await store.database("app", migrations=Path(__file__).parent / "migrations")
```

```go
//go:embed migrations/*.sql
var migrations embed.FS

db, err := sqldb.Database(store, "app", sqldb.Migrations(migrations))
```

```rust
let db = store.database("app").migrations("migrations").open()?;
```

```sql title="migrations/0001_notes.sql"
create table notes (
    id         text primary key,
    author_id  integer not null,
    title      text not null,
    done       integer not null default 0 check (done in (0, 1)),
    tags       text not null default '[]' check (json_valid(tags)),
    created_at integer not null
) strict;
create index notes_author on notes (author_id, created_at);
```

- A database's name is `[a-z0-9][a-z0-9_-]{0,63}`, lower case, so that it
  cannot climb out of `sql/` or meet another name on a file system that folds
  case; its file is `sql/<name>.db` in the store's directory.
- Opening applies the migrations the file has not applied, all in one
  transaction, so the file has the old schema or the whole new one. Foreign
  keys are off while they run and checked before the commit, since rebuilding a
  table with them on deletes its children through `on delete cascade`.
- An applied migration never changes: the file keeps each one's checksum, and
  an edited, renamed or missing migration, a pending one named before the last
  applied, or a file that ran more of them than the program knows, refuses to
  open, naming the migration.
- Migrations are a directory of `.sql` files named `NNNN_what.sql`, or the
  files themselves by name (`{ '0001_notes.sql': text }`) for a program bundled
  into one file. A relative directory is the working directory's, so a program
  names its own, as above. Go takes an `fs.FS`, an `embed.FS` as it is.
- Without `migrations` the file opens as it is, created empty when there is
  none: for scripts and first tries. A program that owns a database passes
  them, so that every start checks the file is what the code expects.
- `await` is where a migration that fails is told: at the start, not at the
  first query. A database is `durability: 'full'`, as kv and jobs are: a write
  that returned survives a power loss.

## Read

```ts
type Note = { id: string; author_id: number; title: string; done: number; tags: string; created_at: number }

const mine = await db.all<Note>`select * from notes where author_id = ${userId} order by created_at desc limit 50`
const note = await db.one<Note>`select * from notes where id = ${id}`          // Note | undefined
const open = await db.scalar<number>`select count(*) from notes where done = 0`
const done = await db.one<Note>`update notes set done = 1 where id = ${id} returning *`
for await (const note of db.each<Note>`select * from notes order by created_at`) {
	await out.write(note)
}
```

```python
mine = await db.all(Note, "select * from notes where author_id = ? order by created_at desc limit 50", user_id)
note = await db.one(Note, "select * from notes where id = ?", note_id)           # Note | None
mine = await db.all(Note, t"select * from notes where author_id = {user_id}")   # Python 3.14
```

```go
mine, err := sqldb.All[Note](ctx, db, `select * from notes where author_id = ? order by created_at desc limit 50`, userID)
note, found, err := sqldb.One[Note](ctx, db, `select * from notes where id = ?`, id)
```

```rust
let mine = db.all::<Note>(sql!("select * from notes where author_id = ? order by created_at desc limit 50", user_id))?;
let note = db.one::<Note>(sql!("select * from notes where id = ?", id))?;      // Option<Note>
let open: i64 = db.scalar(sql!("select count(*) from notes where done = 0"))?;
```

- The verb says what comes back, and the statement what it does: `all` gives
  the rows, `one` one row or none, `scalar` one value, whether the statement
  reads or is a write with `returning`. SQLite says which when it compiles
  the statement, `sqlite3_stmt_readonly`, and the answer is kept by its text:
  a read runs on a reader, which cannot write, and a write on the writer, in
  a savepoint of the shared commit, as `exec`. A write's rows are read to its
  end inside the savepoint and come back once the commit is durable, with
  `exec`'s errors, `conflict` and `outcome unknown` among them; `one` given
  two rows rolls its write back and is `invalid`. `each` reads only. Its tests
  are an insert returning several rows, a constraint failing after rows came,
  `one` given two, a cancel before the statement's end, and a shared commit
  failing after the statement ran, which answers no rows: nothing comes back
  before the commit is durable.
- A value is always a value and never SQL: a template's `${…}`, Python's `?`
  and `{…}` in a `t"…"` string, Go's `?`, and the values after the text in
  Rust's `sql!`, so a value cannot inject SQL. `:name` takes named values from
  one object: `db.all('… where id = :id', { id })`.
- `one` is `undefined` (`None`, `found` false) for no row, and `invalid` for
  two: a query that may find several asks `all` with a `limit`. `scalar` gives
  the value as SQL answers it, `null` for `max` over no rows, and is `invalid`
  for a query that answers no row.
- `all` holds its rows in memory, at most 64 MiB of them, past which it is
  `limit` saying to use `each`. `each` reads a row at a time from one snapshot,
  held at most five seconds; an export longer than that pages by key.
- TypeScript gives a row's values as SQLite keeps them: `done` is `0` or `1`,
  `tags` the JSON's text. A [table](#tables) turns them into what the program
  means.
- In Rust every call blocks its thread, as file access does; async code runs it
  through `spawn_blocking`. The SDKs' calls never block their event loop.

## Write

```ts
const { changes } = await db.exec`update notes set done = 1 where id = ${id} and author_id = ${userId}`
const { lastInsertRowid } = await db.exec`insert into events (kind, at) values (${kind}, ${Date.now()})`

await db.batch([
	sql`insert into orders (id, user_id, total) values (${order.id}, ${userId}, ${order.total})`,
	...order.lines.map(line => sql`insert into order_lines (order_id, sku, qty) values (${order.id}, ${line.sku}, ${line.qty})`),
])
```

```python
done = await db.exec("update notes set done = 1 where id = ? and author_id = ?", note_id, user_id)
await db.batch([sql("insert into orders (id, user_id, total) values (?, ?, ?)", order.id, user_id, order.total), *lines])
```

```go
result, err := db.Exec(ctx, `update notes set done = 1 where id = ? and author_id = ?`, id, userID)
err = db.Batch(ctx, sqldb.SQL(`insert into orders (id, user_id, total) values (?, ?, ?)`, order.ID, userID, order.Total), lines...)
```

```rust
let done = db.exec(sql!("update notes set done = 1 where id = ? and author_id = ?", id, user_id))?;
db.batch([
    sql!("insert into orders (id, user_id, total) values (?, ?, ?)", order.id, user_id, order.total),
    sql!("insert into order_lines (order_id, sku, qty) values (?, ?, ?)", order.id, line.sku, line.qty),
])?;
```

- `exec` returns once the write is durable, with `changes`, the rows it
  changed, and `lastInsertRowid`, SQLite's rowid of its last insert, a
  `number` while it is exact and a `bigint` past 2^53: the table's own key
  only for an `integer primary key`. A key the program chose, a UUID, is the
  key; a key the database made comes back through `returning`.
- Writes from many callers at once share a commit, each in a savepoint of it,
  with one disk sync for all: one that fails rolls back alone. A database takes
  tens of thousands of inserts a second this way where a transaction each
  manages hundreds.
- `batch([…])` writes statements known before they run as one, all of them
  or none, in one savepoint of the next shared commit, so it costs what one
  write does, as D1's, libSQL's and Drizzle's `batch` are one transaction.
  `sql` makes each, as a template or as text and values:
  `sql('… where id = ?', id)`. A write that depends on what a read found is a
  [transaction](#transactions), which holds the writer and pays a sync of its
  own: another verb, since it costs another thing.
- A unique or primary key already held is `conflict`; another constraint, a
  check, a foreign key, a `not null`, is `invalid`. Either says which, in
  `constraint`: `'unique'`, `'primaryKey'`, `'foreignKey'`, `'check'` or
  `'notNull'`, with the table and the constraint when SQLite names them.
- A shared commit whose disk sync fails cannot say whether it was saved: each
  of its writes is `outcome unknown`. Read what you wrote before writing again:
  `stock = stock - 1` sent twice takes two.

## Tables

```ts
type Note = { id: string; author_id: number; title: string; done: boolean; tags: string[]; created_at: Date }

export const notes = db.table<Note>('notes', { types: { done: 'bool', tags: 'json', created_at: 'time' } })

const note = await notes.insert({ id: uuidv7(), author_id: userId, title: 'Buy milk', done: false, tags: ['home'], created_at: new Date() })
await notes.insert(drafts)                                               // several rows: all of them or none
await notes.upsert(note, { conflict: 'id', update: ['title', 'done'], where: { author_id: userId } })
```

```python
notes = db.table("notes", Note)          # a dataclass, a TypedDict or a pydantic model
note = await notes.insert(Note(id=uuid7(), author_id=user_id, title="Buy milk", tags=["home"], created_at=now))
```

```go
notes := sqldb.Table[Note](db, "notes")
note, err := notes.Insert(ctx, Note{ID: uuid.NewV7(), AuthorID: userID, Title: "Buy milk", Tags: []string{"home"}, CreatedAt: store.Now()})
```

```rust
let notes = db.table::<Note>("notes");
let note = notes.insert(&Note { id: Uuid::now_v7(), author_id: user_id, title: "Buy milk".into(), ..Note::default() })?;
```

- A table handle is typed where it opens, as a bucket and a queue are, costs
  nothing and checks nothing until its first call. Open each table once, in
  the module that owns it, and export it: a second handle opened without the
  first one's `types` reads `0` where the type says `false`.
- `insert` writes a row, every field it has, and returns it as the file keeps
  it: the defaults and the generated columns the database filled, a time to the
  millisecond. A list of rows is one write, all of them or none, split into
  statements within SQLite's limit on values. A key that is taken is
  `conflict`.
- `upsert(row, { conflict, update })` inserts the row, or, when the columns
  `conflict` names are taken, sets the columns `update` names from it:
  `insert … on conflict (id) do update set title = excluded.title, …`. `update`
  is required, so that an upsert changes no column it did not name; `where`
  is the condition the row that holds the id must meet, its owner's, so that
  an id from a request changes nothing of another's row: `changes` is `0`.
- A row's names are its columns' names. Python, Go and Rust read into the type
  itself: a Go field `CreatedAt` is the column `created_at`, and a field holding
  a list, a map or a struct is JSON. TypeScript loses the type at run time, so
  `types` names the columns SQLite keeps otherwise, as a bucket's `type` does:
  `'bool'`, an integer `0` or `1`; `'time'`, unix milliseconds read as a
  `Date`; `'json'`, text read as its value; `'bigint'`. A table can also take a
  Standard Schema, as a queue does, which checks each row read.
- `undefined` in a row, as in a condition, is `invalid`: an `update` taken from
  a form's fields must not set a column the form left out to `null`.
- Use UUID version 7 for keys: their first bits are a time, so new rows land
  together in the key's index; 500,000 notes went in 5 to 6.5 times faster than
  with version 4.

## Queries without SQL text

The same query built from pieces, for when its conditions depend on the
request. Every piece is SQL a person would write, and the builder adds only
the `and`s, the parentheses, the placeholders and the quotes around names.

```ts
const orders = db.table<Order>('orders')

const urgent = await orders
	.where({ status: 'pending', deleted: false })
	.where(sql.or(sql`amount >= ${1000}`, { priority: 'high' }))
	.orderBy('created_at', 'desc')
	.limit(50)
	.all()
// select * from "orders" where "deleted" = ? and "status" = ? and (amount >= ? or "priority" = ?)
//   order by "created_at" desc limit cast(? as integer)
```

```python
orders = db.table("orders", Order)

urgent = await (
    orders.where(status="pending", deleted=False)
    .where(sql.or_(sql("amount >= ?", 1000), {"priority": "high"}))
    .order_by("created_at", "desc")
    .limit(50)
    .all()
)
```

```go
orders := sqldb.Table[Order](db, "orders")

urgent, err := orders.
	Where(sqldb.Eq{"status": "pending", "deleted": false}).
	Where(sqldb.Or(sqldb.SQL("amount >= ?", 1000), sqldb.Eq{"priority": "high"})).
	OrderBy("created_at", sqldb.Desc).
	Limit(50).
	All(ctx)
```

```rust
let orders = db.table::<Order>("orders");

let urgent = orders
    .filter(sql!("status = ? and deleted = ?", "pending", false))
    .filter(sql::or([sql!("amount >= ?", 1000), sql!("priority = ?", "high")]))
    .order_by("created_at", Desc)
    .limit(50)
    .all()?;
```

A filter that only applies sometimes is an `if`; a query is a value, and each
call returns a new one, so the first stays as it was:

```ts
let query = orders.where({ tenant_id: tenantId })
if (onlyPaid) query = query.where({ status: 'paid' })
if (minTotal !== undefined) query = query.where(sql`total >= ${minTotal}`)
const page = await query.orderBy('created_at', 'desc').limit(50).all()
const total = await query.count()                 // every match: count leaves out order and limit
```

```go
query := orders.Where(sqldb.Eq{"tenant_id": tenantID})
if onlyPaid {
	query = query.Where(sqldb.Eq{"status": "paid"})
}
page, err := query.OrderBy("created_at", sqldb.Desc).Limit(50).All(ctx)
```

```rust
let mut query = orders.filter(sql!("tenant_id = ?", tenant_id));
if let Some(min) = min_total {
    query = query.filter(sql!("total >= ?", min));
}
let page = query.order_by("created_at", Desc).limit(50).all()?;
```

A condition is a value too: kept in a variable, joined with others, passed to
any query.

```ts
const visible = sql.or({ owner_id: user.id }, { is_public: true })
const shown = sql.and(visible, sql`deleted_at is null`)
const docs = await db.table<Doc>('documents').where(shown).all()
// … where (("owner_id" = ? or "is_public" = ?) and deleted_at is null)
```

Lists, JSON, joins, rows of another shape, related rows, subqueries, a sort a
request chooses, and writes:

```ts
await orders.where({ status: ['pending', 'paid'] }).all()          // status in (…); [] finds nothing
await orders.where(sql`total between ${100} and ${5000}`).count()

await users.where(sql`meta ->> 'plan' = ${'pro'}`).all()           // a field of a JSON column
await notes.where(sql.has('tags', 'urgent')).all()                 // the JSON list tags holds 'urgent'
await users.where({ id }).update({ meta: sql`json_set(meta, '$.plan', ${plan})` })
await posts.where({ id }).update({ views: sql`views + 1` })

const mine = await db.table<Order>('orders as o').join('users as u', 'u.id = o.user_id').where({ 'u.email': email }).all()
const rows = await db.table('orders as o')
	.leftJoin('users as u', sql`u.id = o.user_id and u.active = ${true}`)
	.select<{ id: string; total: number; email: string | null }>('o.id, o.total, u.email')
	.all()
const totals = await orders.select<{ user_id: number; total: number }>('user_id, sum(total) as total').groupBy('user_id').all()

const latest = db.table<Post>('posts as p').select<{ id: string; title: string }>('p.id, p.title')
	.where('p.author_id = u.id').orderBy('p.id', 'desc').limit(3)
const authors = await db.table<User>('users as u').include('posts', latest).where({ 'u.active': true }).all()
// User & { posts: { id: string; title: string }[] }: each user's last three, from the same statement

const paid = db.table('orders as o').select('1').where('o.user_id = u.id').where({ 'o.status': 'paid' })
const buyers = await db.table<User>('users as u').where(sql`exists ${paid}`).all()

const sorts = { newest: ['created_at', 'desc'], largest: ['total', 'desc'] } as const
const sorted = await orders.orderBy(...(sorts[request.sort] ?? sorts.newest)).limit(50).all()

const { changes } = await notes.where({ id, author_id: userId }).update({ done: true })
await notes.where(sql`created_at < ${cutoff}`).delete()
```

- `where` adds a condition with `and`: an object of equal values, a piece of
  SQL made by `sql`, or a group from `sql.or` and `sql.and`. Equal values are
  written in the order of their columns' names, each name quoted, so the same
  filter is the same statement and SQLite compiles it once. Rust spells it
  `filter`, `where` being a keyword there.
- In an object of equal values, `null` is `is null`, and a list is `in (…)`,
  written as one value that SQLite reads with `json_each`, so a list of any
  length is the same statement and keeps its index, and an empty one finds
  nothing. Inside one object the values join with `and`, in `sql.or` too.
- `undefined` is `invalid`, never a filter left out: a missing tenant id must
  not turn a tenant's page into everyone's. A filter that applies sometimes is
  an `if`.
- `sql.or(…)` and `sql.and(…)` put their conditions in parentheses, at any
  depth, their values in the order they are written; an empty `or` finds
  nothing and an empty `and` everything. Python spells them `sql.or_` and
  `sql.and_`, as it spells `with_`.
- A piece of SQL takes its values at `?` alone, and as many as its `?`s,
  counted outside quotes and comments: a piece with too few or too many, or
  with `?1` or `:name`, which would shift when pieces join, is `invalid`. A
  query inside a piece is a subquery in parentheses, its values in place.
- `orderBy(name, 'asc' | 'desc')` and `groupBy(…names)` take column names and
  quote them, each part of a name with a dot on its own, `"u"."created_at"`,
  so a sort cannot be SQL; a sort by an expression is
  ``orderBy(sql`lower(title)`)``. `sql.ident(name)` quotes a name anywhere
  else. Quoting keeps SQL out, not a column the request should not see: a
  request picks from the program's own sorts, as above, since an order shows
  something of what it orders.
- `select`, `join`, `leftJoin` and `having` take SQL text, which TypeScript
  takes only as text written in the code: a `string` that came from elsewhere
  does not compile, and goes through `sql` or `sql.ident`.
- `select<T>` names the shape of the row it gives; Go spells it
  `sqldb.Select[T](query, …)`, a Go method taking no type parameter. A join
  without `select` reads the first table's columns, `o.*`, so that a name both
  tables have cannot take the other's value.
- `include(name, query)` gives each row the rows of a query as a list under
  `name`, its `limit` counted for each row, which it must have. It is planned
  and not in the first build: a list through `json_group_array` loses bytes,
  which JSON cannot hold, and integers past 2^53 in a JSON parser, and a value
  comes back as it went in or not at all. A prototype weighs it against a
  second statement in the same snapshot, `row_number() over (partition by …)`,
  joined in the SDK, by exact values and by SQLite's plan.
- `all`, `one`, `scalar`, `count` and `each` read; `count` counts every match,
  without the order and the limit. `update` and `delete` write and say how many
  rows changed; a value `update` sets may be a piece of SQL. `update` or
  `delete` without a condition, or with one that is empty, `{}` or
  `sql.and()`, is `invalid`: every row is `delete from notes`, said in SQL.
  It stops a mistake, not a caller who means it: `where 1 = 1` passes, and who
  may change what is a condition the program writes.
- `sql.has(name, value)` is a JSON list holding a value, and `->>` reads a
  field of a JSON column, `meta ->> 'plan'`, as SQLite spells it. Neither uses
  an index unless the migrations make one on the expression.
- An array inside a piece of SQL is `invalid` unless it says what it is:
  `sql.list(ids)` for `in (…)` or `sql.json(tags)` for a JSON value. A row's
  field holding an array or an object is JSON.
- A limit is written `limit cast(? as integer)`, since a bare `limit ?` makes
  SQLite compile the statement again at every call.
- `toSQL()` gives the text and the values a query sends, to read or to log;
  `explain()` gives SQLite's plan for it.

### Pages

```ts
const first = await orders.where({ tenant_id: tenantId }).orderBy('created_at', 'desc').orderBy('id', 'desc').list({ limit: 50 })
const second = await orders.where({ tenant_id: tenantId }).orderBy('created_at', 'desc').orderBy('id', 'desc').list({ limit: 50, after: first.next })
// { rows, next }: next is undefined after the last page
```

- `list` reads a page after the last row of the one before, as kv's and jobs'
  `list` do: by the query's order, which ends in a unique column so that no row
  is read twice or skipped while rows come and go. `next` is opaque and holds
  the order's values of the last row with a digest of the query, so that a
  cursor given to another query, another order or other filters, is
  `invalid`. `limit` and `offset` remain, for a page by its number.

The builder lives in each SDK, and the core sees plain SQL: building a query
crosses no pipe and no network. `testdata/sql/queries.json` holds every query
of this book with the text and the values it must give, and the cases a
builder gets wrong first: a `?` in a string or a comment, groups inside
groups, empty lists and groups, a condition in a left join's `on`, a name from
a request, `null`, bytes and integers past 2^53, a `select` that changes the
row. Every SDK's tests read it, as they read the protocol's vectors.

## Transactions

```ts
const placed = await db.tx(async tx => {
	const left = await tx.scalar<number>`select stock from items where sku = ${sku}`
	if (left < 1) return false
	await tx.exec`update items set stock = stock - 1 where sku = ${sku}`
	await tx.table<Order>('orders').insert(order)
	await tx.with(emails).add({ order: order.id, to: user.email })
	return true
})
```

```python
async with db.tx() as tx:
    left = await tx.scalar("select stock from items where sku = ?", sku)
    ...
```

```go
err = db.Tx(ctx, func(tx *sqldb.Tx) error {
	left, err := sqldb.Scalar[int64](ctx, tx, `select stock from items where sku = ?`, sku)
	…
})
```

```rust
let placed = db.tx(|tx| -> Result<bool, ShopError> {
    let left: i64 = tx.scalar(sql!("select stock from items where sku = ?", sku))?;
    if left < 1 {
        return Ok(false);
    }
    tx.exec(sql!("update items set stock = stock - 1 where sku = ?", sku))?;
    tx.with(&emails).add(&Email { order: order.id, to: user.email.clone() })?;
    Ok(true)
})?;
```

- `tx` is for writes that depend on what a read found.
  Returning commits what it wrote; a throw (an error, a panic) rolls back. Its
  function runs once: nothing runs it again, since a call to a payment
  service inside would be made twice.
- It holds the database's writer while the function runs and pays a disk sync
  of its own, so transactions run one at a time and the shared commits of other
  writes wait for it. A read inside sees the transaction's own writes, and two
  transactions cannot both take the last item.
- A transaction holds the writer five seconds at most, in the program's process
  as across the network: past them it rolls back and is `limit`. The core's
  own timer rolls it back and frees the writer, so a client stuck in an
  `await`, or gone, holds nothing past the bound. Inside one,
  call nothing that waits on the world, an HTTP request or a queue's answer: a
  rollback cannot take back what it did.
- `tx.with(handle)` takes a bucket or a queue of the same database in, so that
  a job or a key commits with the rows; a call around the transaction from
  inside it, `db.exec` or `emails.add`, is `invalid`, since it would wait for
  the writer the transaction holds.
- Over the pipe a statement costs microseconds; across a network each costs a
  round trip, so a transaction there keeps to a few statements.

## kv and jobs in the database

```ts
const sessions = db.bucket<Session>('sessions')     // in sql/app.db, beside the tables
const emails = db.queue<Email>('emails')
```

A bucket or a queue opened from a database lives in its file and commits with
its rows in `db.tx`; opened from the store it lives in kv.db or jobs.db, with
a writer of its own (decision 15). The handle and its calls are the same
either way. A queue in the application's file shares the application's
writer, which is the point and its cost.

## Search and geometry

FTS5 and R*Tree are in every connection, as SQLite has them; a migration makes
the index and the triggers that keep it, and a query reads it:

```ts
const hits = await db.all<Message>`select m.* from messages_fts f join messages m on m.id = f.rowid
	where messages_fts match ${query} and m.chat_id = ${chatId} order by bm25(messages_fts) limit 50`
```

The `unicode61` tokenizer folds Cyrillic case but knows neither `ё` nor word
endings: `сообщения` does not find `сообщение`. A search that does is phase 6.

## Values

| Value   | TypeScript                       | Python      | Go              | Rust             | Stored as                    |
|---------|----------------------------------|-------------|-----------------|------------------|------------------------------|
| text    | `string`                         | `str`       | `string`        | `String`, `&str` | `TEXT`                       |
| integer | `number`, `bigint` past 2^53     | `int`       | integers        | integers         | `INTEGER`                    |
| float   | `number`                         | `float`     | `float64`       | `f64`            | `REAL`                       |
| bytes   | `Uint8Array`                     | `bytes`     | `[]byte`        | `Vec<u8>`        | `BLOB`                       |
| boolean | a `'bool'` column's `boolean`    | `bool`      | `bool`          | `bool`           | `INTEGER`, `0` or `1`        |
| time    | a `'time'` column's `Date`       | `datetime`  | `time.Time`     | `SystemTime`     | `INTEGER`, unix milliseconds |
| UUID    | `string`                         | `uuid.UUID` | `uuid.UUID`     | `uuid::Uuid`     | `TEXT`, `8-4-4-4-12`         |
| JSON    | a `'json'` column; `sql.json(v)` | `dict`      | `sqldb.JSON[T]` | `sql::json(&v)`  | `TEXT`, checked `json_valid` |
| nothing | `null`                           | `None`      | a nil pointer   | `None`           | `NULL`                       |

- A value SQLite would change is refused before anything is written: a NaN,
  which a `REAL` column keeps as `NULL`; an unsigned integer past `int64`; a
  TypeScript `number` past 2^53, which already lost its last digits, and goes
  as a `bigint`.
- A value that does not read as its field's type is `invalid`, naming the
  column and the field.
- A time is unix milliseconds, as jobs and blobs carry it; write it from the
  store's clock, which a test moves, rather than from SQL's `unixepoch()`,
  which reads the machine's.

## Errors

| Error             | When                                                                                                                                                                                                                  | What to do                          |
|-------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------------------------------------|
| `invalid`         | two rows for `one`; a value that does not read as its field; a constraint other than a key; `undefined` in a condition or a row; a piece whose values do not match its `?`s; `update` or `delete` without a condition | fix the call                        |
| `conflict`        | a unique or primary key already held                                                                                                                                                                                  | it exists: read it, or pick another |
| `limit`           | `all` past 64 MiB; a transaction past five seconds; the store's memory                                                                                                                                                | read with `each`, or do less        |
| `corrupt`         | a file SQLite finds damaged                                                                                                                                                                                           | restore it from a backup            |
| `closed`          | the store closed                                                                                                                                                                                                      | open it again                       |
| `unavailable`     | another process holds the writer past the busy timeout                                                                                                                                                                | try again                           |
| `outcome unknown` | a shared commit whose disk sync failed                                                                                                                                                                                | read what you wrote, then write     |

Every error names its database, and the migration, table, column or
constraint it is about: `sql app: migration 0002_tags.sql: changed after it
was applied`.

## Bounds

| What                                 | Bound                                     |
|--------------------------------------|-------------------------------------------|
| a database's name                    | `[a-z0-9][a-z0-9_-]{0,63}`                |
| rows `all` holds                     | 64 MiB of values, or the store's memory   |
| an `each` snapshot                   | 5 seconds                                 |
| a transaction                        | 5 seconds                                 |
| writes in one shared commit          | 1,024, or 8 MiB                           |
| a shared commit's hold on the writer | 10 seconds                                |
| readers                              | 8, closed after a minute unused but one   |
| statements compiled a connection     | 128, the least recently used closed first |

## Newcomer check, 10 October

Three Haiku agents at the highest effort, with no other context: one said what
each of 35 call sites does, one chose blind between the spellings in doubt,
one reviewed the draft as a developer choosing between it and Drizzle or
Prisma.

| They found                                                                                   | Change                                                                                                   |
|----------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------|
| `orderBy('created_at desc')` is SQL, so a sort from a request is an injection                | `orderBy(name, 'desc')` and `groupBy(names)` take names and quote them                                   |
| `lastId` read as the new row's key, a UUID, where it is SQLite's rowid                       | `lastInsertRowid`, the word of bun:sqlite, better-sqlite3 and node:sqlite                                |
| `exec` gave counts, a rowid and `returning` rows, three shapes for one verb                  | `exec` counts; `all`, `one` and `scalar` take a write with `returning`                                   |
| `exec([…])` not read as all or nothing                                                       | `batch([…])`, D1's and libSQL's word; a second review kept it apart from `tx`, which costs another thing |
| `upsert(row, { on })` lets an id from a request rewrite another's row, and `on` reads as SQL | `upsert(row, { conflict, update })`, `update` required                                                   |
| `where({})` or `sql.and()` before `delete` deletes every row                                 | `update` and `delete` refuse an empty condition                                                          |
| `update({ title: undefined })` would null a column a form left out                           | `undefined` is `invalid` in a row too                                                                    |
| `{ meta: { plan: 'pro' } }` guessed at: a whole JSON value, a path, or something else        | dropped: a field is `meta ->> 'plan'`, SQLite's own spelling                                             |
| `sql.contains('tags', …)` read as a substring                                                | `sql.has`, a list holding a value                                                                        |
| `sql.id(column)` read as the column `id`                                                     | `sql.ident`                                                                                              |
| `'orders o'` read as a table named "orders o"                                                | `'orders as o'`                                                                                          |
| `include` without a limit can build a JSON of a whole table inside SQLite                    | `include` needs its query's `limit`, which is per row; its execution waits for a prototype               |
| a transaction bounded only across the network                                                | five seconds wherever it runs                                                                            |
| `where_` in Rust read as a typo                                                              | Rust spells it `filter`, as Diesel does; the others keep `where`, SQL's word                             |
| a number past 2^53 rounded before the call                                                   | `invalid`, saying to pass a `bigint`                                                                     |
| `./migrations` relative to what, and two branches both adding `0004`                         | the working directory's, named by the program; a pending migration before an applied one refuses         |
| page and total read as two queries                                                           | `count` documented as every match; `list({ after })` pages by key                                        |

Read right and kept: `store.database`, `sql.or` and `sql.and` (an `all` beside
`query.all()` and an `orWhere` that loses a tenant's filter were the
alternatives), an array as `in (…)` with `[]` finding nothing, an empty `or`
finding nothing, `undefined` refused, `include` (`with` is the transaction's),
`types` beside a table, a transaction that holds the writer rather than one
that runs its function again, `orderBy` and `groupBy`. The reviewer also kept,
as better than Drizzle and Prisma: checked migrations, readers that cannot
write, a `limit` that compiles once, a list as one `json_each` value, `one`
refusing two rows, savepoints in a shared commit, and a call around a
transaction refused rather than run outside it.

## Second review, 10 October

An agent the owner works with read the book after the check.

| It found                                                                        | Change                                                                                   |
|---------------------------------------------------------------------------------|------------------------------------------------------------------------------------------|
| `tx([…])` and `tx(fn)` cost two different things under one word                 | `batch([…])` for the list; `tx` takes a function alone                                   |
| `one` on a write with `returning` changes how statements reach their connection | the routing by `sqlite3_stmt_readonly`, the savepoint, durability and errors are written |
| `include` through JSON loses bytes and integers past 2^53                       | planned, its execution chosen by a prototype                                             |
| `update` in an upsert limits the columns, not whose row they are                | `upsert(…, { where })`, the owner's condition                                            |
| a quoted sort is still a column the request chose                               | the book's request picks from the program's sorts; `"u"."created_at"` quoted by parts    |
| a cursor could be given to another query                                        | a cursor holds a digest of its query, and another's is `invalid`                         |
| an empty condition refused reads as a safety                                    | said to stop a mistake, not to authorize                                                 |

## Open

- **kv's `store.tx` over the wire** is optimistic and runs its function again
  when a read it made changed, where SQL's holds the writer and runs it once.
  They answer different needs; whether kv's changes is a decision of its own,
  apart from building sqldb, and the guides say how each behaves.

- **A row's TypeScript types**: `types` beside each table, or read from the
  migrations, where `check (done in (0, 1))` is a boolean and
  `check (json_valid(tags))` JSON, or written by the command line as types and
  decoders, `tinystore sql types`, in phase 4 with `new`, `status` and `check`
  for migrations.
- **One snapshot for several reads**, a page and its total agreeing:
  `db.read(async r => …)` on a reader, since a `tx` holds the writer.
- **Columns as values**, as Drizzle has them, `select({ id: o.id })` with the
  row's type inferred, need the schema in TypeScript; relations declared once,
  rather than `include`'s `where`, the same.
- **Pieces for `like`** that escape `%` and `_` in a search; operators as
  functions, `gte('total', 100)`, beside SQL's own text.
- A foreign key whose parent is missing and a parent deleted under its children
  are both `invalid` today; SQLite tells them apart only by the statement.
- **Rust's migrations** read from a directory at open, or embedded in the
  binary by a macro, which needs a crate of its own.

## Was, in Go

| Was                                                 | Now                                                       |
|-----------------------------------------------------|-----------------------------------------------------------|
| `sqldb.Open(ctx, store, "app", migrations, nil)`    | `store.database('app', { migrations })`                   |
| `sqldb.Exec` and `ExecOne`, `ExecAll`, `ExecScalar` | `exec`; `one`, `all` and `scalar` take `returning` writes |
| `db.Batch(func(b))`, Bun's and Python's `batch`     | `batch([…])`, a list of statements                        |
| `db.View`                                           | `each` for one snapshot; `db.read` is open                |
| `db.Tx`, Go only                                    | `tx` in every language, over the wire too                 |
| `sqldb.Insert(ctx, db, Notes, note)`                | `notes.insert(note)`, `upsert`                            |
| a query built as a string and its arguments         | `table(…).where(…)`                                       |
| `sqldb.JSONOf(ids)` read with `json_each`           | a list in `where`, or `sql.list(ids)`                     |
| `Options.In` for kv and jobs                        | `db.bucket(…)`, `db.queue(…)`                             |
| "not a query builder"                               | a builder in every SDK, writing the SQL a person would    |
