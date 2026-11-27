# SQL API for Rust

Every public type, trait and function of the SQL engine in `tinystore::sql` and `tinystore::aio::sql`, generated from its source. The [SQL guide](../../guides/sql/README.md) explains how to use them, and the [Bun and Node](../bun/sql.md), [Python](../python/sql.md) and [Go](../go/sql.md) pages list the same API.

## Store

```rust
impl Store
```

### Store.database

```rust
pub fn database(&self, name: &str) -> DatabaseBuilder
```

The application's database `name`, `sql/<name>.db` in the store's
directory, or `local/<name>.db` when no backup takes it, made the
first time it opens:

```rust
let db = store.database("app").migrations("migrations").open()?;
```

## aio::Store

```rust
impl aio::Store
```

### aio::Store.database

```rust
pub fn database(&self, name: &str) -> DatabaseBuilder
```

The application's database `name`, `sql/<name>.db` in the store's
directory, as `crate::Store::database` opens it:

```rust
let db = store.database("app").migrations("migrations").open()?;
```

## Write

```rust
pub struct Write(_)
```

A write of a batch: a statement, a job's add of a queue opened from the
database, `queue.adding(&job)`, or an event's append of a stream opened
from it, `audit.appending(name, &fields)`. A statement becomes one by
`into`:

```rust
let db = store.database("app").open()?;
let indexing = db.queue::<Index>("indexing").open()?;
db.batch([
    tinystore::sql!("update notes set body = ? where id = ?", body, note).into(),
    indexing.adding(&Index { note }), // committed with the row, or not at all
])?;
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
`Store::bucket` keeps one in kv.db: the same
handle with the same calls, whose writes commit with the rows in
`Database::tx` through `tx.with(&bucket)`.

### Database.queue

```rust
pub fn queue<V>(&self, name: &str) -> QueueBuilder<V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
```

A queue of jobs kept in this database's file, as
`Store::queue` keeps one in jobs.db: the same
handle with the same calls and workers, whose jobs commit with the rows
in `Database::tx` through `tx.with(&queue)`. It shares the database's
writer, which is the point and its cost.

### Database.files

```rust
pub fn files(&self, name: &str) -> FilesBuilder<'static>
```

A set of files kept in this database's file, as
`Store::files` keeps one in blobs.db: the same
handle with the same calls, whose writes commit with the rows in
`Database::tx` through `tx.with(&files)`. The bytes too large to keep
inline lie in the folder beside the file, `sql/app.blobs/`.

```rust
let db = store.database("app").open()?;
let packages = db.files("packages").open()?;
db.tx(|tx| {
    tx.exec(tinystore::sql!("insert into plugins (file) values (?)", "stripe-1.0.0.js"))?;
    tx.with(&packages).put("stripe-1.0.0.js", b"export const charges = 1")?;
    Ok::<_, tinystore::Error>(())
})?;
```

### Database.search_index

```rust
pub fn search_index<T: DeserializeOwned + 'static>(&self, name: &str) -> TableIndexBuilder<T>
```

An index of a table of this database, kept in its file: the table's
rows found again by their words, their vectors or both, as
`Store::index` finds documents, each hit's value
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

### Database.events

```rust
pub fn events(&self, stream: &str) -> EventsBuilder<'static>
```

The store's stream of events `stream`, opened through this database:
the same handle as `Store::events` opens, with
the same calls and reads, the stream in records.db where every stream
lives. Its appends inside `Database::tx`, through `tx.with(&audit)`,
and in `Database::batch`, through
`appending`, are kept in this
database's file with the rows and written to the stream once they
commit, before the call returns: an event exists exactly when the
change it records does. An append outside them goes to the stream at
once, and no write is atomic across two files.

```rust
let db = store.database("app").open()?;
let audit = db.events("audit").open()?;
db.tx(|tx| {
    tx.exec(tinystore::sql!("update roles set role = ? where user_id = ?", "admin", 42))?;
    tx.with(&audit).append("role.changed", &RoleChanged { user_id: 42, role: "admin" })?;
    Ok::<_, tinystore::Error>(())
})?;
let page = audit.list().newest().limit(50).page()?; // the line is in the stream
```

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

### Database.each

```rust
pub fn each<T: DeserializeOwned>(&self, statement: impl Into<Sql>) -> Result<Each<T>>
```

The rows a statement gives, one at a time, each read as `all` reads
it: read a part at a time from one snapshot, so that memory holds a
part and not the rows, however many there are. The snapshot lasts
five seconds at most: a walk that takes longer fails `limit`, and is
read a page at a time with a table's `list`, or ordered by its key
with a table's `each`, which reads on past it. A write with
`returning` gives its rows as `all` does.

```rust
for order in db.each::<Order>("select * from orders")? {
    let order = order?;
}
```

### Database.describe

```rust
pub fn describe(&self, statement: impl Into<Sql>) -> Result<Vec<Column>>
```

The columns a statement answers, in its order, each with the type its
table declares where it is a table's own column, whether or not the
statement would answer a row: it is compiled on a reader and nothing
runs. The values of its `?`s are not read, and may be left out. A
statement that writes, `returning` or not, is `invalid`, and one that
names what its file lacks fails as `all` fails it.

```rust
let columns = db.describe("select id, title, length(title) as size from notes where author_id = ?")?;
// id: Some("TEXT"), title: Some("TEXT"), size: None
```

### Database.statement

```rust
pub fn statement(&self, statement: impl Into<Sql>) -> BoundStatement
```

A statement bound to this database, for a stream's `where`, which
keeps a key to the values its one column gives: nothing runs until a
query reads it, once as the query starts, on a reader of this file.

```rust
let db = store.database("app").open()?;
let teachers = db.statement("select id from teachers");
let day = std::time::Duration::from_secs(24 * 60 * 60);
let used = store.events("activity").open()?.list().since(day).filter("userId", &teachers).count()?;
```

### Database.exec

```rust
pub fn exec(&self, statement: impl Into<Sql>) -> Result<Done>
```

Runs a write once it is durable. Writes from many callers share a
commit, each in a savepoint of it: one that fails rolls back alone.

### Database.batch

```rust
pub fn batch<I>(&self, writes: I) -> Result<Vec<Done>>
where
    I: IntoIterator,
    I::Item: Into<Write>,
```

Runs writes known before they run as one, all of them or none, in one
savepoint of the next shared commit: it costs what one write does, and
gives what each did, in their order. A write is a statement, a job's
add of a queue opened from the database, `queue.adding(&job)`, which
commits with the rows and changes one job, or none when its id was
taken, or an event's append of a stream opened from it,
`audit.appending(name, &fields)`, written to the stream before the
batch returns; see `Write`.

### Database.tx

```rust
pub fn tx<T, E: From<Error>>(&self, work: impl FnOnce(&Tx<'_>) -> Result<T, E>) -> Result<T, E>
```

Runs `work` in a transaction that holds the writer, for writes that
depend on what its reads found: `Ok` commits, an error or a panic rolls
back. It runs once. Past five seconds it rolls back and is `limit`.

A bucket or a queue opened from the database joins it through
`tx.with(&handle)`, so that its keys and jobs commit with the rows,
and so does a stream opened from it: its events are written to the
stream once the transaction commits, before this returns.

### Database.mirror

```rust
pub fn mirror<T>(&self, table: &str) -> MirrorBuilder<T>
```

A table that mirrors another system, `T` its rows: its `replace`
takes the source's list of a scope and its `upsert` a change of some
rows, and each answers what was added, changed and gone. The key,
what names a row at the source, is given before it opens.

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

The newest may be drafts, named `0007_messages.draft.sql`: they apply
in a transaction of their own once the file is copied to
`sql/<name>.before-drafts`, and one edited since it was applied
refuses the file, naming `tinystore migrate --redo-drafts`, which
takes it back to the copy and applies them again. A draft renamed
without `.draft`, its text the same, has shipped, and the copy goes.

### DatabaseBuilder.durability

```rust
pub fn durability(self, durability: Durability) -> Self
```

How far this database's commits go before they return, where the
store's own setting is not this file's: what may be lost to a power
loss beside what may not. Its buckets and queues commit with it. A
database open already keeps the durability it was opened with, and
asking for another is `Invalid`.

### DatabaseBuilder.backup

```rust
pub fn backup(self, backup: bool) -> Self
```

Whether backups take the database. `false` keeps its file and the
bytes of its files in `local/<name>.db` and `local/<name>.blobs/`
rather than `sql/`, where no backup looks, this release's or an older
one's, open or not, and no node copies it: each node that opens it
writes its own, and what a backup leaves out nothing else keeps. A
name lives in one folder: one whose file lies in the other is
`Invalid`, naming both.

```rust
let uploads = store.database("uploads").backup(false).open()?;
let pending = uploads.files("pending").ttl(std::time::Duration::from_secs(15 * 60)).open()?;
```

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

## Column

```rust
pub struct Column {
    pub name: String,
    pub declared: Option<String>,
}
```

A column a statement answers: its name as its rows name it, and the type
its table declares for it where the column is a table's own, as SQLite
keeps it: a type a strict table takes in capitals, `INTEGER` or `TEXT`,
and any other as written, `varchar(80)`; `None` for an expression, and
for a column its table declares no type for.

## Each

```rust
pub struct Each<T> { /* private fields */ }
```

The rows of a statement, each as a `T`, as `Database::each` and
`Query::each` give them. Dropped, it stops its read
and gives its reader back.

## Mark

```rust
pub struct Mark { /* private fields */ }
```

A column that marks a row with when it was marked, and the unit it keeps
that time in: `Mark::seconds("deleted_at")`. A column's name alone is a
mark in milliseconds, `"deleted_at".into()`.

### Mark.millis

```rust
pub fn millis(column: impl Into<String>) -> Mark
```

A column that keeps the time in unix milliseconds.

### Mark.seconds

```rust
pub fn seconds(column: impl Into<String>) -> Mark
```

A column that keeps the time in unix seconds, as `unixepoch()` does.

### Mark.column

```rust
pub fn column(&self) -> &str
```

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

## Changed

```rust
pub struct Changed<T> {
    pub before: T,
    pub after: T,
}
```

A row that changed: as the table held it, and as the source gave it.

## Diff

```rust
pub struct Diff<T> {
    pub added: Vec<T>,
    pub changed: Vec<Changed<T>>,
    pub gone: Vec<T>,
    pub same: u64,
    pub suspect: bool,
}
```

What a mirror's call changed, each row a `T`: the rows added, the rows
changed as they were and as they are, the rows the source dropped, and
how many it named as they were held. A suspect answer wrote nothing.

A row answers by the columns the list's rows carry: an added row and a
changed one's `after` as the list gave it, a changed one's `before` and a
gone one as the table held them.

## Guarded

```rust
pub struct Guarded<'m, T> { /* private fields */ }
```

A mirror's `replace` with its min_kept guard; `Mirror::min_kept` makes one.

### Guarded.replace

```rust
pub fn replace<S: AsRef<str>, V: Serialize>(&self, scope: &[(S, V)], rows: &[T]) -> Result<Diff<T>>
```

`Mirror::replace`, which writes nothing when the guard holds it back.

### Guarded.begin

```rust
pub fn begin<S: AsRef<str>, V: Serialize>(&self, scope: &[(S, V)]) -> Result<Run<T>>
```

`Mirror::begin`, whose commit writes nothing when the guard holds it back.

## Mirror

```rust
pub struct Mirror<T> { /* private fields */ }
```

A table that mirrors another system; `Database::mirror` opens one.
Clones share it.

### Mirror.replace

```rust
pub fn replace<S: AsRef<str>, V: Serialize>(&self, scope: &[(S, V)], rows: &[T]) -> Result<Diff<T>>
```

Makes the scope, the rows whose columns hold the scope's values, `&[]`
for the whole table, what `rows` lists, in one write, all of it or
none: adds what the table lacks, writes the compared columns' changes,
and deletes, or marks gone, what the list leaves out. A row the list
names outside the scope is `Invalid`, and so is one whose key holds a
null. Values compare as the table keeps them: a column converts what
it is given as SQLite stores it, `4` given to a text column is `"4"`,
an integer and a real compare by their numbers, and a key's text by
the collation of the index that makes it unique. On a copy of a node
it is `Unavailable`: run it at the file's home.

A scope of mixed types takes `serde_json::Value`s, `json!(1)` beside
`json!("fall")`.

### Mirror.upsert

```rust
pub fn upsert(&self, rows: &[T]) -> Result<Diff<T>>
```

Writes what `rows` adds and changes, a delta of the source's, and
marks nothing gone: its `gone` is always empty.

### Mirror.min_kept

```rust
pub fn min_kept(&self, fraction: f64) -> Guarded<'_, T>
```

The mirror's `replace` guarded against an answer that cannot be
right: when the scope holds live rows and the list would leave fewer
than `fraction` of them live, an empty list among them, it writes
nothing and answers what it would have done, `suspect`. A fraction
outside `(0, 1]` is `Invalid`.

```rust
let diff = works.min_kept(0.5).replace(&[("course_id", 1)], &[])?;
assert!(diff.suspect); // a course that held works: nothing was deleted
```

### Mirror.begin

```rust
pub fn begin<S: AsRef<str>, V: Serialize>(&self, scope: &[(S, V)]) -> Result<Run<T>>
```

Begins a `replace` of the scope whose list comes in parts: each
`Run::add` gives a part, and `Run::commit` makes the scope what
every part listed, in one write, so that only what no part named is
gone. A run dropped before its commit writes nothing. For a list too
large for one call, or one read a page at a time from its source.

```rust
let run = works.begin(&[("course_id", 1)])?;
for page in &pages {
    run.add(page)?;
}
let diff = run.commit()?; // gone: what no page named
```

## MirrorBuilder

```rust
pub struct MirrorBuilder<T> { /* private fields */ }
```

How a mirror opens; `Database::mirror` makes one.

### MirrorBuilder.key

```rust
pub fn key<I: IntoIterator<Item = S>, S: Into<String>>(self, columns: I) -> Self
```

The columns that name a row at the source, `["course_id", "id"]`: a
unique index or the table's primary key must cover them.

### MirrorBuilder.gone

```rust
pub fn gone(self, mark: impl Into<Mark>) -> Self
```

A column that marks what the source dropped with the store's time
instead of deleting the row, an integer of unix milliseconds or, with
`Mark::seconds("gone_at")`, seconds. A row marked is neither marked
nor answered again, and one the source names once more loses its mark
and is added again.

### MirrorBuilder.change_on

```rust
pub fn change_on<I: IntoIterator<Item = S>, S: Into<String>>(self, columns: I) -> Self
```

The columns whose change makes a row changed; every column the rows
carry but the key's when not given. A row whose compared columns are
the same is not written at all, even where its other columns differ:
a `fetched_at` the source writes at every read changes nothing.

### MirrorBuilder.open

```rust
pub fn open(self) -> Result<Mirror<T>>
```

Opens the mirror once its table has every column it names and its
key names one row; `Invalid` otherwise.

## Run

```rust
pub struct Run<T> { /* private fields */ }
```

A mirror's `replace` whose list comes in parts; `Mirror::begin` makes
one. Its parts wait outside memory until the commit; a run dropped
before it, or that no call reached for five minutes, writes nothing.

### Run.add

```rust
pub fn add(&self, rows: &[T]) -> Result<()>
```

Gives a part of the list, checked as a `replace`'s list is: rows of
the first part's shape, inside the scope, each key once in the whole
run. A part that fails ends the run.

### Run.commit

```rust
pub fn commit(self) -> Result<Diff<T>>
```

Makes the scope what every part listed, as `Mirror::replace` makes
it of one list, and answers what changed.

## TxMirror

```rust
pub struct TxMirror<'t, T> { /* private fields */ }
```

A mirror's calls inside a transaction of its database, `tx.with(&works)`:
what they write commits with the transaction or not at all, and they see
what it wrote.

### TxMirror.replace

```rust
pub fn replace<S: AsRef<str>, V: Serialize>(&self, scope: &[(S, V)], rows: &[T]) -> Result<Diff<T>>
```

`Mirror::replace`, inside the transaction.

### TxMirror.upsert

```rust
pub fn upsert(&self, rows: &[T]) -> Result<Diff<T>>
```

`Mirror::upsert`, inside the transaction.

## and

```rust
pub fn and<I>(conditions: I) -> Sql
where
    I: IntoIterator,
    I::Item: Into<Sql>,
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
    I::Item: Into<Sql>,
```

Conditions joined with `or`, in parentheses; an empty `or` finds nothing.

## value

```rust
pub fn value<T: Serialize + ?Sized>(value: &T) -> Sql
```

A value as a piece of SQL, for a column an update sets.

## AtVersion

```rust
pub struct AtVersion<'r, T> { /* private fields */ }
```

The edits of a query made from a version a program saw, from
`Query::if_version`: `tasks.filter(sql::eq("id", &1)).if_version(3).update(…)`.

### AtVersion.update

```rust
pub fn update<I, S>(&self, sets: I) -> Result<Done>
where
    I: IntoIterator<Item = (S, Sql)>,
    S: AsRef<str>,
```

`Query::update` of the rows at the version, all of them or none.

### AtVersion.delete

```rust
pub fn delete(&self) -> Result<Done>
```

`Query::delete` of the rows at the version, all of them or none: a
mark, on a table with a deleted column, which a delete for good is not.

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
`sql::value` or a piece of SQL, and says how many changed. A versioned
table's version goes up by one.

### Query.delete

```rust
pub fn delete(&self) -> Result<Done>
```

Deletes the rows the conditions match, and says how many it deleted.
On a table with a deleted column it marks the live ones instead, with
the store's clock, and raises a versioned table's version.

### Query.with_deleted

```rust
pub fn with_deleted(&self) -> Self
```

The query of every row, those the deleted column marks among them.

### Query.deleted_only

```rust
pub fn deleted_only(&self) -> Self
```

The query of the rows the deleted column marks, and of no other.

### Query.restore

```rust
pub fn restore(&self) -> Result<Done>
```

Clears the mark of the marked rows the conditions match, whichever
rows the handle shows, and says how many came back; a versioned
table's version goes up by one.

### Query.purge

```rust
pub fn purge(&self) -> Result<Done>
```

Deletes for good the rows the conditions match of those the handle
shows: the marked ones of `Query::deleted_only`, or every one of
`Query::with_deleted`. A handle that shows the live rows alone
refuses it, `Invalid`, so that nothing live is purged by a handle
that forgot to ask.

### Query.if_version

```rust
pub fn if_version(&self, version: i64) -> AtVersion<'r, T>
```

The edits of a versioned table made from `version`: they write only
when every row the conditions match is at it, all of those rows, and
otherwise nothing, failing `Conflict` with the version a row is at.
No row at all is no conflict: the edit changes none.

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

### Query.each

```rust
pub fn each(&self) -> Result<Each<T>>
```

The rows one at a time, read a part at a time from one snapshot, so
that memory holds a part and not the rows: for more than fits in
memory. A snapshot lasts five seconds at most. Ordered by columns by
their names, a unique one last, as `list` is, the walk reads on past
that in a snapshot of its own, after the last row it gave, which sees
the rows as they are then; any other walk fails `limit` there. Inside
a transaction it gives the rows `all` reads.

```rust
for order in db.table::<Order>("orders").order_by("id", Direction::Asc).each()? {
    let order = order?;
}
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

impl Deref<Target = Query<'r, T>>
```

A table: its queries, and its rows' inserts and upserts. `Deref`s to the
query of all its rows.

A table may keep two columns of its rows for the program: `deleted`, which
marks a row deleted rather than deleting it, so that every query and write
of the handle passes it by, and `version`, which every update raises, so
that an edit made from an old version is refused rather than written over
another's. A statement written by hand is not rewritten: they are the
handle's.

```rust
let tasks = db.table::<Task>("tasks").deleted(sql::Mark::seconds("deleted_at")).version("version");
let edit = tasks.filter(sql::eq("id", &1)).if_version(0).update([("title", sql::value("from b"))]);
if edit.as_ref().is_err_and(|error| error.kind() == ErrorKind::Conflict) {
    // sql app: table tasks: the row is at version 1, not 0
}
tasks.filter(sql::eq("id", &1)).delete()?; // marks deleted_at; no query of tasks finds it
tasks.deleted_only().filter(sql::eq("id", &1)).restore()?;
```

### Table.name

```rust
pub fn name(&self) -> &str
```

### Table.deleted

```rust
pub fn deleted(self, mark: impl Into<Mark>) -> Self
```

Marks a row deleted rather than deleting it: the mark's column, a
nullable integer, holds when, by the store's clock, in unix
milliseconds or, with `Mark::seconds("deleted_at")`, seconds. Every
query and write of the handle passes the marked rows by, unless
`Query::with_deleted` or `Query::deleted_only` asks for them;
`delete` marks, `Query::restore` clears the mark, and
`Query::purge` deletes for good. A marked row is still in the file
and holds its key, so an insert of that key fails `Conflict`; an
upsert whose key a marked row holds updates that row and clears its
mark: the row came back.

### Table.version

```rust
pub fn version(self, column: &str) -> Self
```

Counts a row's versions in `column`, an integer, not null, default 0:
every update, delete, restore and upsert of the handle raises it by
one, and an edit after `Query::if_version` writes only rows at the
version it names. The handle sets the column itself: an update or an
upsert's update that names it is `Invalid`, and an insert starts a row
at version 0 whatever the row gives for it.

### Table.insert

```rust
pub fn insert(&self, row: &T) -> Result<T>
```

Inserts a row, every field it has, and gives it back as the file keeps
it: the defaults and the generated columns the database filled. On a
versioned table the row starts at version 0, whatever it gives for
the column, since the version is the handle's to count.

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
condition; `None` when the condition kept it. On a table with a
deleted column the update clears a mark, and on a versioned one it
raises the version.

### Table.upsert_all

```rust
pub fn upsert_all(&self, rows: &[T], upsert: Upsert) -> Result<Vec<T>>
```

Upserts rows of one shape as one statement, all of them or none, each
as `Table::upsert` does one. It gives back the rows it wrote as the
file keeps them, without a row whose key is held by a row the
condition kept; SQLite refuses a list that holds one key twice.

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

## BoundStatement

```rust
pub struct BoundStatement { /* private fields */ }
```

A statement bound to the database that runs it, made by
`Database::statement`: what a stream's
`where` keeps a key to, as SQL's `in (select …)` does. A stream lives in
records.db and belongs to no database, so the statement says whose it is.
Nothing runs until a query reads it.

## Sql

```rust
pub struct Sql { /* private fields */ }
```

A statement's text and the values of its `?`s, which are always values and
never SQL, so a value cannot inject any. `sql!` makes one:

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

A database's transaction, from `Database::tx`: its
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
pub fn with<'h, H: TxHandle>(&self, handle: &'h H) -> H::InTx
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

## aio::sql::Each

```rust
pub struct Each<T> { /* private fields */ }
```

The rows of a statement, each as a `T`, as
`Database::each` and
`Query::each` give them: a `Stream` that begins its
walk at its first poll. Dropped, it stops its read at the next row and
gives its reader back. A failure is its last item.

## aio::sql::DatabaseBuilder

```rust
pub struct DatabaseBuilder { /* private fields */ }
```

How a database opens; `Store::database` makes
one, with the blocking builder's options.

### aio::sql::DatabaseBuilder.migrations

```rust
pub fn migrations(self, migrations: impl Into<Migrations>) -> Self
```

The migrations the file must have applied, as
`sql::DatabaseBuilder::migrations` takes them.

### aio::sql::DatabaseBuilder.durability

```rust
pub fn durability(self, durability: Durability) -> Self
```

How far its commits go before they return, as
`sql::DatabaseBuilder::durability` says.

### aio::sql::DatabaseBuilder.open

```rust
pub fn open(self) -> Result<Database>
```

Opens the database as `sql::DatabaseBuilder::open` does: a plain
call, made once at start, since a first open applies and checks the
migrations.

## aio::sql::Database

```rust
pub struct Database { /* private fields */ }
```

An application's database whose calls are awaited: the same file,
writer and readers as `sql::Database`, and the same database when one
is made from the other. Clones share it, and it closes with its store.

### aio::sql::Database.mirror

```rust
pub fn mirror<T>(&self, table: &str) -> MirrorBuilder<T>
```

A table that mirrors another system, as `sql::Database::mirror`
opens one: its `replace` and `upsert` awaited.

### aio::sql::Database.name

```rust
pub fn name(&self) -> &str
```

### aio::sql::Database.blocking

```rust
pub fn blocking(&self) -> &Database
```

The same database, for its blocking calls.

### aio::sql::Database.statement

```rust
pub fn statement(&self, statement: impl Into<Sql>) -> BoundStatement
```

A statement bound to this database, for a stream's `where`, as
`sql::Database::statement` binds one: nothing runs until a query
reads it, as it starts on the store's pool.

### aio::sql::Database.all

```rust
pub fn all<T>(
    &self,
    statement: impl Into<Sql>,
) -> impl Future<Output = Result<Vec<T>>> + Send + 'static
where
    T: DeserializeOwned + Send + 'static,
```

The rows a statement gives, each read as a `T`, as
`sql::Database::all` reads them. A statement that writes, as one
with `returning` does, is queued at the writer by the read that finds
it writes, and from then on it commits whether or not its future is
still awaited.

### aio::sql::Database.one

```rust
pub fn one<T>(
    &self,
    statement: impl Into<Sql>,
) -> impl Future<Output = Result<Option<T>>> + Send + 'static
where
    T: DeserializeOwned + Send + 'static,
```

The one row a statement gives, or none, as `sql::Database::one`
reads it; it runs as `Database::all` does.

### aio::sql::Database.scalar

```rust
pub fn scalar<T>(
    &self,
    statement: impl Into<Sql>,
) -> impl Future<Output = Result<T>> + Send + 'static
where
    T: DeserializeOwned + Send + 'static,
```

The one value of the one row a statement gives, as
`sql::Database::scalar` reads it; it runs as `Database::all` does.

### aio::sql::Database.describe

```rust
pub fn describe(
    &self,
    statement: impl Into<Sql>,
) -> impl Future<Output = Result<Vec<Column>>> + Send + 'static
```

The columns a statement answers, in its order, with the types their
tables declare, as `sql::Database::describe` says them: compiled on
a reader on the store's pool, and nothing of it run.

### aio::sql::Database.each

```rust
pub fn each<T>(&self, statement: impl Into<Sql>) -> Each<T>
where
    T: DeserializeOwned + Send + 'static,
```

The rows a statement gives, one at a time, as `sql::Database::each`
walks them: a `Stream` whose parts are read on the store's pool from
one snapshot, each row decoded as it is polled, so that memory holds
a part and not the rows. Its walk begins at its first poll; past five
seconds of one snapshot it fails `limit`.

### aio::sql::Database.exec

```rust
pub fn exec(&self, statement: impl Into<Sql>) -> impl Future<Output = Result<Done>> + Send + 'static
```

Runs a write once it is durable, sharing a commit with the writes of
other callers, as `sql::Database::exec` does. On a copy it is passed
to the home from a thread of its own, which waits for the home's
answer.

### aio::sql::Database.batch

```rust
pub fn batch<I>(&self, writes: I) -> impl Future<Output = Result<Vec<Done>>> + Send + 'static
where
    I: IntoIterator,
    I::Item: Into<Write>,
```

Runs writes known before they run as one, all of them or none, as
`sql::Database::batch` does: statements, and jobs' adds of queues
opened from the database, `queue.adding(&job)`.

### aio::sql::Database.tx

```rust
pub fn tx<T, E, F>(&self, work: F) -> impl Future<Output = Result<T, E>> + Send + 'static
where
    F: FnOnce(&Tx<'_>) -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: From<Error> + Send + 'static,
```

Runs `work` in a transaction that holds the writer, as
`sql::Database::tx` does, on a thread of the store's pool: `work` is
blocking, so it must not wait for the program's awaits, and it runs on
no executor's thread. Inside, a bucket or a queue opened from the
database joins it through `tx.with(&handle)`.

The database's transactions wait in one line, one at a time in the
order they were polled, whether awaited or asked for over the wire;
those waiting hold no thread. Its five seconds count from its first
poll, its wait in the line among them. A panic in `work` rolls the
transaction back and is resumed at the `.await`.

```rust
let booked = db
    .tx(move |tx| -> tinystore::Result<bool> {
        let left: i64 = tx.scalar(sql!("select seats_left from flights where id = ?", flight_id))?;
        if left == 0 {
            return Ok(false); // sold out: nothing is written
        }
        tx.exec(sql!("update flights set seats_left = seats_left - 1 where id = ?", flight_id))?;
        Ok(true)
    })
    .await?;
```

### aio::sql::Database.table

```rust
pub fn table<T>(&self, name: &str) -> Table<T>
```

A table of the database, typed where it opens, as
`sql::Database::table` gives one: its inserts and upserts, and its
queries, each awaited. It costs nothing until its first call.

### aio::sql::Database.bucket

```rust
pub fn bucket<V: Value>(&self, name: &str) -> BucketBuilder<V>
```

A bucket kept in this database's file, as `Database::bucket` opens
one, its calls awaited: its writes commit with the rows in
`tx` through `tx.with(&bucket)`.

### aio::sql::Database.events

```rust
pub fn events(&self, stream: &str) -> EventsBuilder<'_>
```

The store's stream `stream` opened through this database, as
`Database::events` opens it, its calls awaited: its appends commit
with the rows in `tx` through
`tx.with(&audit)`, and in `batch`
through `appending`.

### aio::sql::Database.files

```rust
pub fn files(&self, name: &str) -> FilesBuilder<'_>
```

A set of files kept in this database's file, as `Database::files`
opens one, its calls awaited: its writes commit with the rows in
`tx` through `tx.with(&files)`.

### aio::sql::Database.queue

```rust
pub fn queue<V>(&self, name: &str) -> QueueBuilder<V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
```

A queue kept in this database's file, as `Database::queue` opens
one, its calls awaited: its jobs commit with the rows in
`tx` through `tx.with(&queue)`, and
in `batch` through
`adding`.

### aio::sql::Database.search_index

```rust
pub fn search_index<T: DeserializeOwned + Send + 'static>(&self, name: &str) -> TableIndexBuilder<T>
```

An index of a table of this database, as `Database::search_index`
opens one, its calls awaited. Its rows change with the table's, in
their commit; a search inside `tx`
goes through `tx.with(&index)` and sees what the transaction wrote.

### aio::sql::Database.drop_search_index

```rust
pub fn drop_search_index(&self, name: &str) -> impl Future<Output = Result<bool>> + Send + 'static
```

Drops the table's index `name` this database keeps, as
`Database::drop_search_index` does. It runs in one transaction, so
it waits in the database's line with its transactions, on no thread,
and once polled it runs to its end: the triggers come off the table
in one commit or not at all, whether or not its future is still awaited.

## aio::sql::Query

```rust
pub struct Query<T> { /* private fields */ }
```

A query on a table whose calls are awaited, built as
`sql::Query` is: each step returns a new query, so that the first
stays as it was. `all`, `one`, `scalar`, `count`, `explain` and `list`
read on the store's pool; `update` and `delete` write.

```rust
let urgent: Vec<Order> = db
    .table::<Order>("orders")
    .filter(sql::eq("status", "pending"))
    .order_by("created_at", Direction::Desc)
    .limit(50)
    .all()
    .await?;
```

### aio::sql::Query.filter

```rust
pub fn filter(&self, condition: impl Into<Sql>) -> Self
```

Adds a condition with `and`, as `sql::Query::filter` does.

### aio::sql::Query.select

```rust
pub fn select<S>(&self, columns: impl Into<Sql>) -> Query<S>
```

The columns a row holds, as SQL text, and the shape of the row they
make, as `sql::Query::select` says.

### aio::sql::Query.include

```rust
pub fn include<C>(&self, name: &str, query: &Query<C>) -> Self
```

Gives each row the rows of another query as a list under `name`, as
`sql::Query::include` does, in the same statement.

### aio::sql::Query.join

```rust
pub fn join(&self, table: &str, on: impl Into<Sql>) -> Self
```

Joins a table, `"users as u"`, on a condition.

### aio::sql::Query.left_join

```rust
pub fn left_join(&self, table: &str, on: impl Into<Sql>) -> Self
```

Joins a table whose columns may be null where no row of it meets the condition.

### aio::sql::Query.group_by

```rust
pub fn group_by<I: IntoIterator<Item = S>, S: AsRef<str>>(&self, columns: I) -> Self
```

Groups the rows by columns, quoted as names.

### aio::sql::Query.having

```rust
pub fn having(&self, condition: impl Into<Sql>) -> Self
```

A condition on the groups, as `filter` takes one.

### aio::sql::Query.order_by

```rust
pub fn order_by(&self, column: &str, direction: Direction) -> Self
```

Orders the rows by a column, quoted as a name.

### aio::sql::Query.order_by_sql

```rust
pub fn order_by_sql(&self, piece: impl Into<Sql>, direction: Direction) -> Self
```

Orders the rows by a piece of SQL: `sql!("lower(title)")`.

### aio::sql::Query.limit

```rust
pub fn limit(&self, rows: u64) -> Self
```

### aio::sql::Query.offset

```rust
pub fn offset(&self, rows: u64) -> Self
```

### aio::sql::Query.to_sql

```rust
pub fn to_sql(&self) -> Result<Sql>
```

The text and the values the query sends, to read or to log.

### aio::sql::Query.count

```rust
pub fn count(&self) -> impl Future<Output = Result<u64>> + Send + 'static
```

How many rows the conditions match, or how many groups, as
`sql::Query::count` counts them.

### aio::sql::Query.explain

```rust
pub fn explain(&self) -> impl Future<Output = Result<Vec<String>>> + Send + 'static
```

SQLite's plan for the query, a line a step.

### aio::sql::Query.update

```rust
pub fn update<I, S>(&self, sets: I) -> impl Future<Output = Result<Done>> + Send + 'static
where
    I: IntoIterator<Item = (S, Sql)>,
    S: AsRef<str>,
```

Sets columns of the rows the conditions match, as
`sql::Query::update` does, and says how many changed.

### aio::sql::Query.delete

```rust
pub fn delete(&self) -> impl Future<Output = Result<Done>> + Send + 'static
```

Deletes the rows the conditions match, or marks them on a table with
a deleted column, as `sql::Query::delete` does, and says how many.

### aio::sql::Query.with_deleted

```rust
pub fn with_deleted(&self) -> Self
```

The query of every row, as `sql::Query::with_deleted` reads them.

### aio::sql::Query.deleted_only

```rust
pub fn deleted_only(&self) -> Self
```

The query of the marked rows alone, as `sql::Query::deleted_only` reads them.

### aio::sql::Query.restore

```rust
pub fn restore(&self) -> impl Future<Output = Result<Done>> + Send + 'static
```

Clears the mark of the marked rows the conditions match, as
`sql::Query::restore` does.

### aio::sql::Query.purge

```rust
pub fn purge(&self) -> impl Future<Output = Result<Done>> + Send + 'static
```

Deletes for good what the handle shows of the rows the conditions
match, as `sql::Query::purge` does; refused on the live rows alone.

### aio::sql::Query.if_version

```rust
pub fn if_version(&self, version: i64) -> AtVersion<T>
```

The edits made from `version`, as `sql::Query::if_version` gives them.

### aio::sql::Query.all

```rust
pub fn all(&self) -> impl Future<Output = Result<Vec<T>>> + Send + 'static
```

### aio::sql::Query.one

```rust
pub fn one(&self) -> impl Future<Output = Result<Option<T>>> + Send + 'static
```

### aio::sql::Query.scalar

```rust
pub fn scalar<V: DeserializeOwned + Send + 'static>(
    &self,
) -> impl Future<Output = Result<V>> + Send + 'static
```

### aio::sql::Query.list

```rust
pub fn list(
    &self,
    limit: u64,
    after: Option<&str>,
) -> impl Future<Output = Result<Page<T>>> + Send + 'static
```

A page after the last row of the one before, by the query's order, as
`sql::Query::list` reads one; its `next` reads the next page, and
only of this query.

### aio::sql::Query.each

```rust
pub fn each(&self) -> Each<T>
```

The rows one at a time, as `sql::Query::each` walks them: a `Stream`
whose parts are read on the store's pool, each row decoded as it is
polled. Ordered by columns by their names, a unique one last, it reads
on past one snapshot's five seconds.

```rust
let mut orders = db.table::<Order>("orders").order_by("id", Direction::Asc).each();
while let Some(order) = orders.next().await {
    let order = order?;
}
```

## aio::sql::Table

```rust
pub struct Table<T> { /* private fields */ }

impl Deref<Target = Query<T>>
```

A table whose calls are awaited: its queries, and its rows' inserts and
upserts, as `sql::Table` has them. `Deref`s to the query of all its
rows.

### aio::sql::Table.name

```rust
pub fn name(&self) -> &str
```

### aio::sql::Table.deleted

```rust
pub fn deleted(self, mark: impl Into<Mark>) -> Self
```

Marks a row deleted rather than deleting it, as `sql::Table::deleted` does.

### aio::sql::Table.version

```rust
pub fn version(self, column: &str) -> Self
```

Counts a row's versions, as `sql::Table::version` does.

### aio::sql::Table.insert

```rust
pub fn insert(&self, row: &T) -> impl Future<Output = Result<T>> + Send + 'static
```

Inserts a row and gives it back as the file keeps it, as
`sql::Table::insert` does. The row is encoded when the call is made.

### aio::sql::Table.insert_all

```rust
pub fn insert_all(&self, rows: &[T]) -> impl Future<Output = Result<Vec<T>>> + Send + 'static
```

Inserts rows of one shape as one write, all of them or none, as
`sql::Table::insert_all` does.

### aio::sql::Table.upsert

```rust
pub fn upsert(
    &self,
    row: &T,
    upsert: Upsert,
) -> impl Future<Output = Result<Option<T>>> + Send + 'static
```

Inserts a row, or updates the row that holds its key, as
`sql::Table::upsert` does; `None` when the upsert's condition kept
that row.

### aio::sql::Table.upsert_all

```rust
pub fn upsert_all(
    &self,
    rows: &[T],
    upsert: Upsert,
) -> impl Future<Output = Result<Vec<T>>> + Send + 'static
```

Upserts rows of one shape as one statement, all of them or none, as
`sql::Table::upsert_all` does. The rows are encoded when the call is
made.

## aio::sql::AtVersion

```rust
pub struct AtVersion<T> { /* private fields */ }
```

The awaited edits made from a version a program saw, as
`sql::AtVersion` has them.

### aio::sql::AtVersion.update

```rust
pub fn update<I, S>(&self, sets: I) -> impl Future<Output = Result<Done>> + Send + 'static
where
    I: IntoIterator<Item = (S, Sql)>,
    S: AsRef<str>,
```

An update of the rows at the version, all of them or none.

### aio::sql::AtVersion.delete

```rust
pub fn delete(&self) -> impl Future<Output = Result<Done>> + Send + 'static
```

A delete, a mark on a table with a deleted column, of the rows at the
version, all of them or none.

## aio::sql::Guarded

```rust
pub struct Guarded<'m, T> { /* private fields */ }
```

An awaited mirror's `replace` with its min_kept guard; `Mirror::min_kept`
makes one.

### aio::sql::Guarded.replace

```rust
pub fn replace<S: AsRef<str>, V: Serialize>(
    &self,
    scope: &[(S, V)],
    rows: &[T],
) -> impl Future<Output = Result<Diff<T>>> + Send + 'static
```

`Mirror::replace`, which writes nothing when the guard holds it back.

### aio::sql::Guarded.begin

```rust
pub fn begin<S: AsRef<str>, V: Serialize>(&self, scope: &[(S, V)]) -> Result<Run<T>>
```

`Mirror::begin`, whose commit writes nothing when the guard holds it back.

## aio::sql::Mirror

```rust
pub struct Mirror<T> { /* private fields */ }
```

A table that mirrors another system whose calls are awaited, the same
mirror as `sql::Mirror`. Each call checks and converts its rows when it
is made, so that its future borrows nothing, and writes in its first poll.

### aio::sql::Mirror.replace

```rust
pub fn replace<S: AsRef<str>, V: Serialize>(
    &self,
    scope: &[(S, V)],
    rows: &[T],
) -> impl Future<Output = Result<Diff<T>>> + Send + 'static
```

The scope made what `rows` lists, as `sql::Mirror::replace` makes it.

### aio::sql::Mirror.min_kept

```rust
pub fn min_kept(&self, fraction: f64) -> Guarded<'_, T>
```

`replace` with its min_kept guard, as `sql::Mirror::min_kept` guards it.

### aio::sql::Mirror.begin

```rust
pub fn begin<S: AsRef<str>, V: Serialize>(&self, scope: &[(S, V)]) -> Result<Run<T>>
```

A `replace` whose list comes in parts, as `sql::Mirror::begin`
begins one: its table checked now, its parts and commit awaited.

### aio::sql::Mirror.upsert

```rust
pub fn upsert(&self, rows: &[T]) -> impl Future<Output = Result<Diff<T>>> + Send + 'static
```

What `rows` adds and changes, as `sql::Mirror::upsert` writes it.

## aio::sql::MirrorBuilder

```rust
pub struct MirrorBuilder<T> { /* private fields */ }
```

How an awaited mirror opens, with the blocking builder's options.

### aio::sql::MirrorBuilder.key

```rust
pub fn key<I: IntoIterator<Item = S>, S: Into<String>>(self, columns: I) -> Self
```

The columns that name a row at the source, as `sql::MirrorBuilder::key` takes them.

### aio::sql::MirrorBuilder.gone

```rust
pub fn gone(self, mark: impl Into<Mark>) -> Self
```

The column that marks what the source dropped, as `sql::MirrorBuilder::gone` says.

### aio::sql::MirrorBuilder.change_on

```rust
pub fn change_on<I: IntoIterator<Item = S>, S: Into<String>>(self, columns: I) -> Self
```

The columns whose change makes a row changed, as `sql::MirrorBuilder::change_on` says.

### aio::sql::MirrorBuilder.open

```rust
pub fn open(self) -> Result<Mirror<T>>
```

Opens the mirror as `sql::MirrorBuilder::open` does: a plain call,
which reads the table's columns and indexes.

## aio::sql::Run

```rust
pub struct Run<T> { /* private fields */ }
```

An awaited mirror's `replace` in parts, the same run as `sql::Run`.

### aio::sql::Run.add

```rust
pub fn add(&self, rows: &[T]) -> impl Future<Output = Result<()>> + Send + 'static
```

Gives a part of the list, as `sql::Run::add` does, checked when
the call is made.

### aio::sql::Run.commit

```rust
pub fn commit(self) -> impl Future<Output = Result<Diff<T>>> + Send + 'static
```

Makes the scope what every part listed, as `sql::Run::commit`
does, its answer decoded on the store's pool.

<!-- Generated by task reference from crates/tinystore through rustdoc. Edit the doc comments there, not this file. -->
