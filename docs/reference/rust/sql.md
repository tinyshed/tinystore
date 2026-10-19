# SQL API for Rust

Every public type, trait and function of the SQL engine in `tinystore::sql`, generated from its source. The [SQL guide](../../sql/README.md) explains how to use them, and the [Bun and Node](../bun/sql.md), [Python](../python/sql.md) and [Go](../go/sql.md) pages list the same API.

## Store

```rust
impl Store
```

### Store.database

```rust
pub fn database(&self, name: &str) -> DatabaseBuilder
```

The application's database `name`, `sql/<name>.db` in the store's
directory, made the first time it opens:

```rust
let db = store.database("app").migrations("migrations").open()?;
```

## Database

```rust
pub struct Database { /* private fields */ }
```

An application's database: its reads, its writes, its transactions.
Clones share the database, which closes with its store.

### Database.bucket

```rust
pub fn bucket<V: Value>(&self, name: &str) -> BucketBuilder<V>
```

A bucket of values by key kept in this database's file, as
[`Store::bucket`](crate::Store::bucket) keeps one in kv.db: the same
handle with the same calls, whose writes commit with the rows in
`Database::tx` through `tx.with(&bucket)`.

### Database.queue

```rust
pub fn queue<V>(&self, name: &str) -> QueueBuilder<V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
```

A queue of jobs kept in this database's file, as
[`Store::queue`](crate::Store::queue) keeps one in jobs.db: the same
handle with the same calls and workers, whose jobs commit with the rows
in `Database::tx` through `tx.with(&queue)`. It shares the database's
writer, which is the point and its cost.

### Database.search_index

```rust
pub fn search_index<T: DeserializeOwned + 'static>(&self, name: &str) -> TableIndexBuilder<T>
```

An index of a table of this database, kept in its file: the table's
rows found again by their words, their vectors or both, as
[`Store::index`](crate::Store::index) finds documents, each hit's value
its row as `Database::all` reads one. The table owns the rows and the
index follows them: every change of a row, by any statement, trigger
or migration, reaches the index in the commit that makes it or not at
all, and one by a process beside the store at the store's next write
or within a second.

```rust
let db = store.database("app").migrations("migrations").open()?;
let articles =
    db.search_index::<Article>("articles_search").table("articles").field("title", 2.0).field("body", 1.0).open()?;
let found = articles.search("новый заголовок").run()?;
```

### Database.drop_search_index

```rust
pub fn drop_search_index(&self, name: &str) -> Result<bool>
```

Drops the table's index `name` this database keeps: its triggers come
off the table in one commit and its rows go in the background; says
whether there was one. A handle of it is refused from then on.

### Database.name

```rust
pub fn name(&self) -> &str
```

### Database.all

```rust
pub fn all<T: DeserializeOwned>(&self, statement: impl Into<Sql>) -> Result<Vec<T>>
```

The rows a statement gives, each read as a `T`: a struct by its
columns' names, a tuple by their order, a single value when a row has
one column. A write with `returning` runs on the writer and gives its
rows once its commit is durable.

### Database.one

```rust
pub fn one<T: DeserializeOwned>(&self, statement: impl Into<Sql>) -> Result<Option<T>>
```

The one row a statement gives, or none; a statement that gives two is
`invalid`, and a write that gave them rolls back.

### Database.scalar

```rust
pub fn scalar<T: DeserializeOwned>(&self, statement: impl Into<Sql>) -> Result<T>
```

The one value of the one row a statement gives, as `count(*)` does;
`Option` takes a `NULL`, as `max` over no rows gives.

### Database.exec

```rust
pub fn exec(&self, statement: impl Into<Sql>) -> Result<Done>
```

Runs a write once it is durable. Writes from many callers share a
commit, each in a savepoint of it: one that fails rolls back alone.

### Database.batch

```rust
pub fn batch<I>(&self, statements: I) -> Result<Vec<Done>>
where
    I: IntoIterator,
    <I as >::Item: Into<Sql>,
```

Runs statements known before they run as one, all of them or none, in
one savepoint of the next shared commit: it costs what one write does.

### Database.tx

```rust
pub fn tx<T, E: From<Error>>(&self, work: impl FnOnce(&Tx<'_>) -> Result<T, E>) -> Result<T, E>
```

Runs `work` in a transaction that holds the writer, for writes that
depend on what its reads found: `Ok` commits, an error or a panic rolls
back. It runs once. Past five seconds it rolls back and is `limit`.

A bucket or a queue opened from the database joins it through
`tx.with(&handle)`, so that its keys and jobs commit with the rows.

### Database.table

```rust
pub fn table<T>(&self, name: &str) -> Table<'static, T>
```

A table of the database, typed where it opens: its rows' inserts and
upserts, and queries built without SQL text. It costs nothing until
its first call.

## DatabaseBuilder

```rust
pub struct DatabaseBuilder { /* private fields */ }
```

How a database opens; `Store::database` makes one.

### DatabaseBuilder.migrations

```rust
pub fn migrations(self, migrations: impl Into<Migrations>) -> Self
```

The migrations the file must have applied: opening applies those it
has not, all in one transaction, and refuses a file that applied an
edited, renamed or missing one, or more than the program has. Without
them the file opens as it is.

### DatabaseBuilder.durability

```rust
pub fn durability(self, durability: Durability) -> Self
```

How far this database's commits go before they return, where the
store's own setting is not this file's: what may be lost to a power
loss beside what may not. Its buckets and queues commit with it. A
database open already keeps the durability it was opened with, and
asking for another is `Invalid`.

### DatabaseBuilder.open

```rust
pub fn open(self) -> Result<Database>
```

## Done

```rust
pub struct Done {
    pub changes: u64,
    pub last_insert_rowid: i64,
}
```

What a write changed: the rows, and SQLite's rowid of the row it inserted,
`0` when it inserted none. The rowid is a table's key only for an `integer
primary key`.

## Migrations

```rust
pub enum Migrations {
    Dir(PathBuf),
    Files(Vec<(String, String)>),
}
```

A database's migrations: a directory of `.sql` files named
`0001_notes.sql`, `0002_tags.sql`, applied in the order of their numbers,
or the files themselves by name, for a program that carries them in its
binary:

```rust
let db = store.database("app").migrations("migrations").open()?;
let db = store
    .database("app")
    .migrations([("0001_notes.sql", "create table notes (id text primary key, title text not null) strict")])
    .open()?;
```

## and

```rust
pub fn and<I>(conditions: I) -> Sql
where
    I: IntoIterator,
    <I as >::Item: Into<Sql>,
```

Conditions joined with `and`, in parentheses, for a group inside an `or`;
an empty `and` finds everything.

## eq

```rust
pub fn eq<T: Serialize + ?Sized>(column: &str, value: &T) -> Sql
```

A column equal to a value, its name quoted; `None` is `is null`.

## has

```rust
pub fn has<T: Serialize + ?Sized>(column: &str, value: &T) -> Sql
```

A JSON list in `column` holding `value`.

## ident

```rust
pub fn ident(name: &str) -> Sql
```

A name, quoted, never SQL: a column a request chooses.

## is_in

```rust
pub fn is_in<T: Serialize>(column: &str, values: &[T]) -> Sql
```

A column holding one of `values`: `in (…)`, written as one value SQLite
reads with `json_each`, so a list of any length is the same statement and
an empty one finds nothing.

## json

```rust
pub fn json<T: Serialize + ?Sized>(value: &T) -> Sql
```

A value kept as its JSON.

## list

```rust
pub fn list<T: Serialize>(values: &[T]) -> Sql
```

A list for `in (…)`, as one JSON value SQLite reads with `json_each`.

## or

```rust
pub fn or<I>(conditions: I) -> Sql
where
    I: IntoIterator,
    <I as >::Item: Into<Sql>,
```

Conditions joined with `or`, in parentheses; an empty `or` finds nothing.

## value

```rust
pub fn value<T: Serialize + ?Sized>(value: &T) -> Sql
```

A value as a piece of SQL, for a column an update sets.

## Direction

```rust
pub enum Direction {
    Asc,
    Desc,
}
```

The way an order goes.

## Page

```rust
pub struct Page<T> {
    pub rows: Vec<T>,
    pub next: Option<String>,
}
```

A page of a query's rows, and the cursor that reads the next; none after
the last.

## Query

```rust
pub struct Query<'r, T> { /* private fields */ }
```

A query on a table: a value, each call returning a new one, so that the
first stays as it was. `all`, `one`, `scalar`, `count` and `list` read;
`update` and `delete` write.

```rust
let urgent: Vec<Order> = db
    .table::<Order>("orders")
    .filter(sql::eq("status", "pending"))
    .filter(sql::or([tinystore::sql!("amount >= ?", 1000), sql::eq("priority", "high")]))
    .order_by("created_at", Direction::Desc)
    .limit(50)
    .all()?;
```

### Query.filter

```rust
pub fn filter(&self, condition: impl Into<Sql>) -> Self
```

Adds a condition with `and`: a piece from `sql!`, `sql::eq`, `sql::or`
and the rest. `filter`, since `where` is Rust's own word.

### Query.select

```rust
pub fn select<S>(&self, columns: impl Into<Sql>) -> Query<'r, S>
```

The columns a row holds, as SQL text, and the shape of the row they make.

### Query.include

```rust
pub fn include<C>(&self, name: &str, query: &Query<'_, C>) -> Self
```

Gives each row the rows of another query as a list under `name`, a
field of the row's type, from the same statement: the query names the
row it belongs to in its `filter`, by this table's alias, names its
columns with `select`, and has a `limit`, which counts for each row.
Every value comes back as it is kept.

```rust
let latest = db
    .table::<Post>("posts as p")
    .select::<Post>("p.id, p.title")
    .filter("p.author_id = u.id")
    .order_by("p.id", Direction::Desc)
    .limit(3);
let authors: Vec<Author> = db.table::<Author>("users as u").include("posts", &latest).all()?;
```

### Query.join

```rust
pub fn join(&self, table: &str, on: impl Into<Sql>) -> Self
```

Joins a table, `"users as u"`, on a condition.

### Query.left_join

```rust
pub fn left_join(&self, table: &str, on: impl Into<Sql>) -> Self
```

Joins a table whose columns may be null where no row of it meets the condition.

### Query.group_by

```rust
pub fn group_by<I: IntoIterator<Item = S>, S: AsRef<str>>(&self, columns: I) -> Self
```

Groups the rows by columns, quoted as names.

### Query.having

```rust
pub fn having(&self, condition: impl Into<Sql>) -> Self
```

A condition on the groups, as `filter` takes one.

### Query.order_by

```rust
pub fn order_by(&self, column: &str, direction: Direction) -> Self
```

Orders the rows by a column, quoted as a name, so that a sort a
request chooses cannot be SQL.

### Query.order_by_sql

```rust
pub fn order_by_sql(&self, piece: impl Into<Sql>, direction: Direction) -> Self
```

Orders the rows by a piece of SQL: `sql!("lower(title)")`.

### Query.limit

```rust
pub fn limit(&self, rows: u64) -> Self
```

### Query.offset

```rust
pub fn offset(&self, rows: u64) -> Self
```

### Query.to_sql

```rust
pub fn to_sql(&self) -> Result<Sql>
```

The text and the values the query sends, to read or to log.

### Query.count

```rust
pub fn count(&self) -> Result<u64>
```

How many rows the conditions match, or how many groups with
`group_by`, without the order and the limit.

### Query.explain

```rust
pub fn explain(&self) -> Result<Vec<String>>
```

SQLite's plan for the query, a line a step.

### Query.update

```rust
pub fn update<I, S>(&self, sets: I) -> Result<Done>
where
    I: IntoIterator<Item = (S, Sql)>,
    S: AsRef<str>,
```

Sets columns of the rows the conditions match, each a value from
`sql::value` or a piece of SQL, and says how many changed.

### Query.delete

```rust
pub fn delete(&self) -> Result<Done>
```

Deletes the rows the conditions match, and says how many it deleted.

### Query.all

```rust
pub fn all(&self) -> Result<Vec<T>>
```

### Query.one

```rust
pub fn one(&self) -> Result<Option<T>>
```

### Query.scalar

```rust
pub fn scalar<V: DeserializeOwned>(&self) -> Result<V>
```

### Query.list

```rust
pub fn list(&self, limit: u64, after: Option<&str>) -> Result<Page<T>>
```

A page after the last row of the one before, by the query's order,
which ends in a unique column so that no row is read twice or skipped;
`next` reads the next page, and only of this query.

## Table

```rust
pub struct Table<'r, T> { /* private fields */ }
```

A table: its queries, and its rows' inserts and upserts. `Deref`s to the
query of all its rows.

### Table.name

```rust
pub fn name(&self) -> &str
```

### Table.insert

```rust
pub fn insert(&self, row: &T) -> Result<T>
```

Inserts a row, every field it has, and gives it back as the file keeps
it: the defaults and the generated columns the database filled.

### Table.insert_all

```rust
pub fn insert_all(&self, rows: &[T]) -> Result<Vec<T>>
```

Inserts rows of one shape as one write, all of them or none.

### Table.upsert

```rust
pub fn upsert(&self, row: &T, upsert: Upsert) -> Result<Option<T>>
```

Inserts a row, or, when its key is taken, sets the columns `update`
names on the row that holds it, if that row meets the upsert's
condition; `None` when the condition kept it.

## Upsert

```rust
pub struct Upsert { /* private fields */ }
```

What an upsert does when the row's key is taken:
`Upsert::on("id").update(["title"]).only_if(sql::eq("author_id", &user_id))`.

### Upsert.on

```rust
pub fn on<I: IntoIterator<Item = S>, S: Into<String>>(conflict: I) -> Upsert
```

The columns whose value, taken, makes the insert an update: the key.

### Upsert.update

```rust
pub fn update<I: IntoIterator<Item = S>, S: Into<String>>(self, columns: I) -> Upsert
```

The columns the update sets from the row; none other changes.

### Upsert.only_if

```rust
pub fn only_if(self, condition: impl Into<Sql>) -> Upsert
```

The condition the row that holds the key must meet, its owner's, or
it stays as it is.

## Sql

```rust
pub struct Sql { /* private fields */ }
```

A statement's text and the values of its `?`s, which are always values and
never SQL, so a value cannot inject any. [`sql!`](crate::sql!) makes one:

```rust
let title: Option<String> = db.one(tinystore::sql!("select title from notes where id = ?", id))?;
```

Text alone is a statement without values: `db.scalar("select count(*) from notes")`.

### Sql.new

```rust
pub fn new(text: impl Into<String>) -> Sql
```

### Sql.bind

```rust
pub fn bind<T: Serialize + ?Sized>(self, value: &T) -> Sql
```

The value of the next `?`. A value SQLite would change, a NaN or an
integer past `i64`, makes the statement `invalid` when it runs.

### Sql.text

```rust
pub fn text(&self) -> &str
```

## Tx

```rust
pub struct Tx<'t> { /* private fields */ }
```

A database's transaction, from [`Database::tx`](super::Database::tx): its
reads see its writes, and a call that fails leaves it as it was before the
call.

### Tx.table

```rust
pub fn table<T>(&self, name: &str) -> Table<'t, T>
```

A table inside the transaction: its calls are the transaction's.

### Tx.all

```rust
pub fn all<T: DeserializeOwned>(&self, statement: impl Into<Sql>) -> Result<Vec<T>>
```

The rows a statement gives, each read as a `T`: a struct by its
columns' names, a tuple by their order, a single value when a row has
one column. A write with `returning` runs on the writer and gives its
rows once its commit is durable.

### Tx.one

```rust
pub fn one<T: DeserializeOwned>(&self, statement: impl Into<Sql>) -> Result<Option<T>>
```

The one row a statement gives, or none; a statement that gives two is
`invalid`, and a write that gave them rolls back.

### Tx.scalar

```rust
pub fn scalar<T: DeserializeOwned>(&self, statement: impl Into<Sql>) -> Result<T>
```

The one value of the one row a statement gives, as `count(*)` does;
`Option` takes a `NULL`, as `max` over no rows gives.

### Tx.exec

```rust
pub fn exec(&self, statement: impl Into<Sql>) -> Result<Done>
```

### Tx.with

```rust
pub fn with<'h, H: TxHandle>(&self, handle: &'h H) -> <H as >::InTx
```

The calls of a bucket or a queue opened from this database, inside the
transaction: what they write commits with the rows or not at all.

```rust
let db = store.database("app").open()?;
let emails = db.queue::<Email>("emails").open()?;
db.tx(|tx| -> tinystore::Result<()> {
    let order: i64 = tx.scalar(tinystore::sql!("insert into orders (total) values (?) returning id", 70))?;
    tx.with(&emails).add(&Email { order })?; // sent only if the order is there
    Ok(())
})?;
```

<!-- Generated by task reference from crates/tinystore through rustdoc. Edit the doc comments there, not this file. -->
