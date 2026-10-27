# SQL API for Python

Every public class, function and type of the SQL engine in `tinystore`, generated from its source. The [SQL guide](../../guides/sql/README.md) explains how to use them, and the [Bun and Node](../bun/sql.md), [Go](../go/sql.md) and [Rust](../rust/sql.md) pages list the same API.

## Sql

```python
@dataclass(frozen=True, slots=True)
class Sql:
    text: str
    values: tuple[SqlArg, ...] = ()
    bare: bool = False  # whether it stands beside others in an and or an or without parentheses: one column's comparison
```

A piece of SQL and the values of its ``?``s, which are always values and never SQL.

``sql`` makes one; a call, a batch and a table's query take it.

## sql

```python
sql = SqlMaker()
```

## Done

```python
@dataclass(frozen=True, slots=True)
class Done:
    changes: int
    last_insert_rowid: int
```

What a write changed: the rows, and SQLite's rowid of the row it inserted, 0 when none.

## Database

```python
class Database:
    name: str
```

An application's database: ``await store.database("app", migrations=...)``.

The verb says what comes back: all the rows, one or none, one value, or
what a write changed. A write with ``returning`` goes through a read's
verb and runs on the writer, its rows given once it is durable.

### Database.describe

```python
@property
def describe() -> str: ...
```

### Database.handle

```python
async def handle(connection: Connection) -> int: ...
```

### Database.keep

```python
def keep(
    method: int,
    body: bytes,
    make: Callable[[Connection], Awaitable[bytes]],
) -> Opener: ...
```

### Database.via

```python
def via(what: str) -> Via: ...
```

### Database.refuse_around

```python
def refuse_around(what: str) -> None: ...
```

Refuses a call made around the database's transaction from inside its function.

It would wait for the writer the transaction holds until the store
rolled the transaction back, and then commit alone.

### Database.bucket

```python
@overload
def bucket[T](
    name: str,
    of: type[T],
    *,
    ttl: Duration | None = None,
    idle: Duration | None = None,
    encrypted: bool = False,
) -> Bucket[T]: ...
@overload
def bucket(
    name: str,
    of: None = None,
    *,
    ttl: Duration | None = None,
    idle: Duration | None = None,
    encrypted: bool = False,
) -> Bucket[Any]: ...
def bucket(
    name: str,
    of: Any = None,
    *,
    ttl: Duration | None = None,
    idle: Duration | None = None,
    encrypted: bool = False,
) -> Bucket[Any]: ...
```

A bucket of values by key kept in this database's file, beside its tables.

The same calls as ``store.bucket``'s, and its writes commit with the
rows in ``db.tx``, through ``tx.with_(bucket)``:

    sessions = db.bucket("sessions", Session, idle="30d")

### Database.files

```python
def files(
    name: str,
    *,
    ttl: Duration | None = None,
    max_file_size: Size | None = None,
) -> Files: ...
```

A set of files kept in this database's file, as ``store.files`` keeps one in blobs.db.

The same calls, and its writes commit with the rows in ``db.tx``,
through ``tx.with_(files)``. A file's bytes past 16 KiB lie in the
folder beside the database's file:

    packages = db.files("packages", max_file_size="10MiB")

### Database.search_index

```python
@overload
def search_index[T](
    name: str,
    of: type[T],
    *,
    table: str,
    key: str | None = None,
    fields: Fields,
    filters: Sequence[str] = (),
    typo_tolerance: bool | None = None,
    vectors: Vectors | None = None,
    hybrid: Hybrid | None = None,
    rebuild: Literal['automatic', 'manual'] | None = None,
) -> TableIndex[T]: ...
@overload
def search_index(
    name: str,
    of: None = None,
    *,
    table: str,
    key: str | None = None,
    fields: Fields,
    filters: Sequence[str] = (),
    typo_tolerance: bool | None = None,
    vectors: Vectors | None = None,
    hybrid: Hybrid | None = None,
    rebuild: Literal['automatic', 'manual'] | None = None,
) -> TableIndex[dict[str, Any]]: ...
def search_index(
    name: str,
    of: Any = None,
    *,
    table: str,
    key: str | None = None,
    fields: Fields,
    filters: Sequence[str] = (),
    typo_tolerance: bool | None = None,
    vectors: Vectors | None = None,
    hybrid: Hybrid | None = None,
    rebuild: Literal['automatic', 'manual'] | None = None,
) -> TableIndex[Any]: ...
```

An index of a table of this database, kept in its file: its rows found again by their words or vectors.

Each hit's value is its row as ``all`` reads it. The table owns the
rows and the index follows them: every change of a row, by any
statement, trigger or migration, reaches it in the commit that makes
it, and one by a process beside the store within a second:

    articles = db.search_index("articles_search", Article, table="articles", fields={"title": 2, "body": 1})

### Database.drop_search_index

```python
async def drop_search_index(name: str) -> bool: ...
```

Drops a table's index this database keeps, its triggers off the table; says whether there was one.

### Database.queue

```python
@overload
def queue[T](
    name: str,
    of: type[T],
    *,
    attempts: int | None = None,
    backoff: Backoff | None = None,
    timeout: Duration | None = None,
    concurrency: int | Concurrency | None = None,
    rate: str | None = None,
    dedupe: Duration | None = None,
    keep: Duration | None = None,
    max_waiting: int | None = None,
    group_rate: str | None = None,
) -> Queue[T]: ...
@overload
def queue(
    name: str,
    of: None = None,
    *,
    attempts: int | None = None,
    backoff: Backoff | None = None,
    timeout: Duration | None = None,
    concurrency: int | Concurrency | None = None,
    rate: str | None = None,
    dedupe: Duration | None = None,
    keep: Duration | None = None,
    max_waiting: int | None = None,
    group_rate: str | None = None,
) -> Queue[Any]: ...
def queue(
    name: str,
    of: Any = None,
    *,
    attempts: int | None = None,
    backoff: Backoff | None = None,
    timeout: Duration | None = None,
    concurrency: int | Concurrency | None = None,
    rate: str | None = None,
    dedupe: Duration | None = None,
    keep: Duration | None = None,
    max_waiting: int | None = None,
    group_rate: str | None = None,
) -> Queue[Any]: ...
```

A queue of jobs kept in this database's file, as ``store.queue`` keeps one in jobs.db.

The same calls and workers, and its jobs commit with the rows in
``db.tx``, through ``tx.with_(queue)``. It shares the database's
writer, which is the point and its cost.

### Database.all

```python
@overload
async def all[T](of: type[T], statement: Statement, /, *values: Any) -> list[T]: ...
@overload
async def all(statement: Statement, /, *values: Any) -> list[dict[str, Any]]: ...
async def all(first: Any, /, *rest: Any) -> list[Any]: ...
```

Every row a statement gives: ``await db.all(Note, "select * from notes where author_id = ?", user_id)``.

### Database.one

```python
@overload
async def one[T](of: type[T], statement: Statement, /, *values: Any) -> T | None: ...
@overload
async def one(statement: Statement, /, *values: Any) -> dict[str, Any] | None: ...
async def one(first: Any, /, *rest: Any) -> Any: ...
```

The one row a statement gives, or None; two are InvalidError, and a write that gave them rolls back.

### Database.scalar

```python
@overload
async def scalar[T](of: type[T], statement: Statement, /, *values: Any) -> T: ...
@overload
async def scalar(statement: Statement, /, *values: Any) -> Any: ...
async def scalar(first: Any, /, *rest: Any) -> Any: ...
```

The one value of the one row a statement gives, as count(*) does; None as SQL answers it.

### Database.columns

```python
async def columns(statement: Statement, /, *values: Any) -> dict[str, list[Any]]: ...
```

What ``all`` gives, by its columns: a list a column, by the column's name.

For rows by the thousand, a chart's points: they come faster, and
nobody turns rows into columns after.

### Database.each

```python
@overload
def each[T](of: type[T], statement: Statement, /, *values: Any) -> AsyncIterator[T]: ...
@overload
def each(statement: Statement, /, *values: Any) -> AsyncIterator[dict[str, Any]]: ...
def each(first: Any, /, *rest: Any) -> AsyncIterator[Any]: ...
```

The rows a statement gives, one at a time as their parts arrive, from one snapshot.

### Database.table

```python
@overload
def table[T](name: str, of: type[T]) -> Table[T]: ...
@overload
def table(name: str, of: None = None) -> Table[dict[str, Any]]: ...
def table(name: str, of: Any = None) -> Table[Any]: ...
```

A table of the database, typed where it opens: its rows' inserts and upserts, and queries.

It costs nothing until its first call; open each once, in the module
that owns it: ``notes = db.table("notes", Note)``.

### Database.exec

```python
async def exec(statement: Statement, /, *values: Any) -> Done: ...
```

Runs a write once it is durable; writes from many callers share a commit, each failing alone.

### Database.batch

```python
async def batch(writes: Sequence[Sql | Adding]) -> list[Done]: ...
```

Writes known before they run, written as one: all of them or none, in a shared commit.

A write is a statement, or a job's add of a queue opened from the
database, ``queue.adding``, which commits with the rows and changed 1
job, or 0 when its id was taken. The answer says what each did, in
their order.

    await db.batch([sql("update notes set body = ? where id = ?", body, id), indexing.adding(Index(id))])

### Database.tx

```python
async def tx[R](fn: Callable[[SqlTx], Awaitable[R]]) -> R: ...
```

Runs fn in a transaction that holds the writer, for writes that depend on what its reads found.

Returning commits, an exception rolls back. It runs once, five seconds
at most, past which the store rolls it back; inside it, call nothing
that waits on the world.

### Database.rows

```python
async def rows(statement: Sql, want: Want) -> Answered: ...
```

### Database.exec_piece

```python
async def exec_piece(statement: Sql) -> Done: ...
```

### Database.each_piece

```python
def each_piece(statement: Sql) -> AsyncIterator[dict[str, SqlValue]] | None: ...
```

## TxQueue

```python
class TxQueue[T]:
    ...
```

A queue's calls inside a database's transaction, from ``tx.with_(queue)``.

A job it adds is there only if the transaction commits, and the queue's
workers find it once it has. A worker, a watch and a page work outside.

### TxQueue.add

```python
async def add(value: T, **options: Any) -> bool: ...
```

Adds a job unless its id is taken, as queue.add does, with the transaction.

### TxQueue.set

```python
async def set(id: str, value: T, **options: Any) -> None: ...
```

### TxQueue.update

```python
async def update(id: str, value: T, **options: Any) -> bool: ...
```

### TxQueue.cancel

```python
async def cancel(id: str) -> bool: ...
```

### TxQueue.get

```python
async def get(id: str) -> Job[T] | None: ...
```

The job under id as the transaction sees it, its own writes among it.

## SqlTx

```python
class SqlTx:
    ...
```

A database's transaction, from ``db.tx``.

Its reads see its writes, and a call that fails leaves it as it was
before the call. Its calls go one at a time, in order.

### SqlTx.describe

```python
@property
def describe() -> str: ...
```

### SqlTx.with_

```python
@overload
def with_[T](handle: Bucket[T]) -> Bucket[T]: ...
@overload
def with_[T](handle: Queue[T]) -> TxQueue[T]: ...
@overload
def with_[T](handle: Channel[T]) -> TxChannel[T]: ...
@overload
def with_(handle: Files) -> Files: ...
def with_(
    handle: Bucket[Any] | Queue[Any] | Channel[Any] | Files,
) -> Bucket[Any] | TxQueue[Any] | TxChannel[Any] | Files: ...
```

A bucket, a queue or files opened from this database, or a channel, inside the transaction.

What it writes commits with the rows or not at all, and what it reads
sees what the transaction wrote; a message is handed over at the commit.

### SqlTx.all

```python
@overload
async def all[T](of: type[T], statement: Statement, /, *values: Any) -> list[T]: ...
@overload
async def all(statement: Statement, /, *values: Any) -> list[dict[str, Any]]: ...
async def all(first: Any, /, *rest: Any) -> list[Any]: ...
```

### SqlTx.one

```python
@overload
async def one[T](of: type[T], statement: Statement, /, *values: Any) -> T | None: ...
@overload
async def one(statement: Statement, /, *values: Any) -> dict[str, Any] | None: ...
async def one(first: Any, /, *rest: Any) -> Any: ...
```

### SqlTx.scalar

```python
@overload
async def scalar[T](of: type[T], statement: Statement, /, *values: Any) -> T: ...
@overload
async def scalar(statement: Statement, /, *values: Any) -> Any: ...
async def scalar(first: Any, /, *rest: Any) -> Any: ...
```

### SqlTx.exec

```python
async def exec(statement: Statement, /, *values: Any) -> Done: ...
```

### SqlTx.table

```python
@overload
def table[T](name: str, of: type[T]) -> Table[T]: ...
@overload
def table(name: str, of: None = None) -> Table[dict[str, Any]]: ...
def table(name: str, of: Any = None) -> Table[Any]: ...
```

A table inside the transaction: its calls are the transaction's.

### SqlTx.rows

```python
async def rows(statement: Sql, want: Want) -> Answered: ...
```

### SqlTx.exec_piece

```python
async def exec_piece(statement: Sql) -> Done: ...
```

### SqlTx.each_piece

```python
def each_piece(statement: Sql) -> AsyncIterator[dict[str, SqlValue]] | None: ...
```

### SqlTx.end

```python
async def end(commit: bool) -> None: ...
```

Sends the last call, commit or roll back, and waits for the store's word.

A commit that left and lost its connection before the answer may have
committed: its outcome is unknown. One lost before it left was rolled
back by the store, and is the loss itself.

### SqlTx.nested

```python
async def nested(method: int, body: bytes) -> bytes: ...
```

A call of kv or jobs in the transaction: its method and its request, and the method's answer.

## RowPage

```python
@dataclass(frozen=True, slots=True)
class RowPage[T]:
    rows: list[T]
    next: str | None
```

A page of a query's rows, and the cursor that reads the next; None after the last.

## Query

```python
class Query[T]:
    ...
```

A query on a table: a value, each call returning a new one, so that the first stays as it was.

``all``, ``one``, ``scalar``, ``count``, ``each`` and ``list`` read;
``update`` and ``delete`` write.

### Query.where

```python
def where(
    condition: Condition | str | None = None,
    /,
    *values: Any,
    **equal: Any,
) -> Query[T]: ...
```

Adds a condition with and.

Equal values, as keywords or a dict, None as is null and a list as in
(...); a piece of SQL from ``sql``; or text and the values of its ``?``s.

### Query.select

```python
@overload
def select[R](of: type[R], columns: str | Sql, /) -> Query[R]: ...
@overload
def select(columns: str | Sql, /) -> Query[dict[str, Any]]: ...
def select(first: Any, /, *rest: Any) -> Query[Any]: ...
```

The columns a row holds, as SQL text, and the type of the row they make.

### Query.include

```python
def include(name: str, query: Query[Any]) -> Query[T]: ...
```

Gives each row the rows of another query as a list under name, from the same statement.

The query names the row it belongs to in its where, by this table's
alias, names its columns with select, and has a limit, which counts
for each row. Every value comes back as it is kept.

### Query.join

```python
def join(table: str, on: str | Sql) -> Query[T]: ...
```

Joins a table, "users as u", on a condition as SQL text or a piece of SQL.

### Query.left_join

```python
def left_join(table: str, on: str | Sql) -> Query[T]: ...
```

Joins a table whose columns may be None where no row of it meets the condition.

### Query.group_by

```python
def group_by(*columns: str) -> Query[T]: ...
```

Groups the rows by columns, quoted as names.

### Query.having

```python
def having(condition: Condition | str, /, *values: Any) -> Query[T]: ...
```

A condition on the groups, as where takes one.

### Query.order_by

```python
def order_by(column: str | Sql, direction: Direction = 'asc') -> Query[T]: ...
```

Orders the rows by a column, quoted as a name, or by a piece of SQL: order_by(sql("lower(title)")).

### Query.limit

```python
def limit(rows: int) -> Query[T]: ...
```

### Query.offset

```python
def offset(rows: int) -> Query[T]: ...
```

### Query.to_sql

```python
def to_sql() -> tuple[str, list[SqlArg]]: ...
```

The text and the values the query sends, to read or to log.

### Query.embedded

```python
def embedded() -> Sql: ...
```

The query inside another piece of SQL: a subquery, in parentheses.

### Query.parts

```python
@property
def parts() -> _Parts: ...
```

### Query.of

```python
@property
def of() -> Any: ...
```

### Query.all

```python
async def all() -> list[T]: ...
```

### Query.one

```python
async def one() -> T | None: ...
```

### Query.scalar

```python
async def scalar(of: Any = None) -> Any: ...
```

### Query.count

```python
async def count() -> int: ...
```

Every row the conditions match, without the order and the limit.

### Query.each

```python
async def each() -> AsyncIterator[T]: ...
```

The rows one at a time, as their parts arrive.

### Query.list

```python
async def list(*, limit: int = 100, after: str | None = None) -> RowPage[T]: ...
```

A page after the last row of the one before, by the query's order.

The order ends in a unique column so that no row is read twice or
skipped; next reads the next page, and only of this query.

### Query.explain

```python
async def explain() -> list[str]: ...
```

SQLite's plan for the query, a line a step.

### Query.update

```python
async def update(
    values: Mapping[str, Any] | None = None,
    /,
    **columns: Any,
) -> Done: ...
```

Sets columns of the rows the conditions match, and says how many changed.

### Query.delete

```python
async def delete() -> Done: ...
```

Deletes the rows the conditions match, and says how many it deleted.

## Table

```python
class Table[T](Query[T]):
    ...
```

A table: its queries, and its rows' inserts and upserts.

``notes = db.table("notes", Note)``, a dataclass, a TypedDict or a
pydantic model, whose fields are the table's columns.

### Table.insert

```python
@overload
async def insert(row: T | Mapping[str, Any], /) -> T: ...
@overload
async def insert(rows: Sequence[T | Mapping[str, Any]], /) -> list[T]: ...
async def insert(given: Any, /) -> Any: ...
```

Inserts a row and gives it back as the file keeps it; a list of rows is one write, all or none.

### Table.upsert

```python
async def upsert(
    row: T | Mapping[str, Any],
    *,
    conflict: str | Sequence[str],
    update: Sequence[str],
    where: Condition | None = None,
) -> T | None: ...
```

Inserts a row, or, when its key is taken, sets the columns update names on the row that holds it.

It does so when that row meets where, and gives back the row as the
file keeps it, or None when where kept it.

<!-- Generated by task reference from sdk/python/src/tinystore/sql.py, sdk/python/src/tinystore/query.py. Edit the doc comments there, not this file. -->
