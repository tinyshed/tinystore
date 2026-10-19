# SQL API for Bun and Node

Every public class, function and type of the SQL engine in `@tinyshed/tinystore`, generated from its source. The [SQL guide](../../sql/README.md) explains how to use them, and the [Python](../python/sql.md), [Go](../go/sql.md) and [Rust](../rust/sql.md) pages list the same API.

## Columns

```ts
type Columns<T> = {
    [K in keyof T]: T[K][]
}
```

A row type's columns: an array of each of its fields.

## Row

```ts
type Row = Record<string, SqlValue>
```

A row as SQLite gave it, by its columns' names.

## Migrations

```ts
type Migrations = string | Record<string, string> | readonly {
    name: string
    sql: string
}[]
```

A database's migrations: a directory of .sql files named 0001_notes.sql,
0002_tags.sql, or the files themselves by name, for a program bundled into
one file. A relative directory is the working directory's.

## DatabaseOptions

```ts
interface DatabaseOptions {
    /** applied when the file has not, and checked at every open; without them the file opens as it is */
    migrations?: Migrations
    /**
     * How far this database's commits go before they return, where the
     * store's own is not this file's: `'os'` for what may be lost to a power
     * loss, beside a store of what may not. Its buckets and queues commit
     * with it. A database open already keeps what it was opened with.
     */
    durability?: Durability
}
```

## Done

```ts
interface Done {
    changes: number
    lastInsertRowid: number | bigint
}
```

What a write changed: the rows, and SQLite's rowid of the row it inserted, 0 when none.

## Statement

```ts
type Statement = [text: string | TemplateStringsArray | Sql, ...values: unknown[]]
```

A statement in any of the ways a call takes it: a template whose values are
its values, the text and the values of its ?s, the text and one object of
named values, or a piece of SQL from `sql`.

    db.all`select * from notes where id = ${id}`
    db.all('select * from notes where id = ?', id)
    db.all('select * from notes where id = :id', { id })

## Sql

```ts
class Sql {
    readonly text: string
    readonly values: readonly SqlArg[]
    readonly bare: boolean
}
```

A piece of SQL and the values of its ?s, which are always values and never
SQL. `sql` makes one; a call, a batch and a table's query take it.

## SqlTag

```ts
interface SqlTag {
    (strings: TemplateStringsArray, ...values: unknown[]): Sql
    (text: string, ...values: unknown[]): Sql
    /** conditions joined with or, in parentheses; an empty or finds nothing */
    or(...conditions: Condition[]): Sql
    /** conditions joined with and, in parentheses; an empty and finds everything */
    and(...conditions: Condition[]): Sql
    /** a list for in (…), as one value SQLite reads with json_each: any length, the same statement */
    list(values: readonly unknown[]): Sql
    /** a value kept as its JSON */
    json(value: unknown): Sql
    /** a name, quoted, never SQL: a column a request chooses */
    ident(name: string): Sql
    /** a JSON list in column `name` holding `value` */
    has(name: string, value: unknown): Sql
}
```

A piece of SQL: `` sql`total >= ${min}` ``, or `sql('total >= ?', min)`.
Its helpers build the pieces a query's conditions are made of.

## sql

```ts
const sql: SqlTag
```

## Database

```ts
class Database implements Runner, Home {
    readonly name: string
    readonly describe: string
}
```

An application's database: `await store.database('app', { migrations })`.
The verb says what comes back: all the rows, one or none, one value, or
what a write changed. A write with `returning` goes through a read's verb
and runs on the writer, its rows given once it is durable.

### Database.handle

```ts
handle(connection: Connection): Promise<number>
```

### Database.keep

```ts
keep(method: number, bytes: Uint8Array, make: (connection: Connection) => Promise<Uint8Array>): Open
```

### Database.via

```ts
via(what: string): Via
```

### Database.bucket

```ts
bucket<T = unknown>(name: string, options?: BucketOptions): Bucket<T>
```

A bucket of values by key kept in this database's file, beside its
tables, as `store.bucket` keeps one in kv.db: the same calls, and its
writes commit with the rows in `db.tx`, through `tx.with(bucket)`.

    const sessions = db.bucket<Session>('sessions', { idle: '30d' })

### Database.queue

```ts
queue<S extends StandardSchemaV1>(name: string, options: QueueOptions & {
        schema: S
    }): Queue<StandardSchemaV1.InferOutput<S>>
queue<T = unknown>(name: string, options?: QueueOptions): Queue<T>
```

A queue of jobs kept in this database's file, as `store.queue` keeps one
in jobs.db: the same calls and workers, and its jobs commit with the
rows in `db.tx`, through `tx.with(queue)`. It shares the database's
writer, which is the point and its cost.

    const emails = db.queue<Email>('emails', { attempts: 5 })

### Database.searchIndex

```ts
searchIndex<T = Row>(name: string, options: TableIndexOptions): TableIndex<T>
```

An index of a table of this database, kept in its file: its rows found
again by their words, vectors or both, each hit's value its row as
`all` reads it. The table owns the rows and the index follows them:
every change of a row, by any statement, trigger or migration, reaches
it in the commit that makes it, and one by a process beside the store
within a second.

    const articles = db.searchIndex<Article>('articles_search', { table: 'articles', fields: { title: 2, body: 1 } })

### Database.dropSearchIndex

```ts
dropSearchIndex(name: string): Promise<boolean>
```

Drops a table's index this database keeps, its triggers off the table; says whether there was one.

### Database.all

```ts
all<T = Row>(...statement: Statement): Promise<T[]>
```

Every row a statement gives: `db.all<Note>\`select * from notes where author_id = ${id}\``

### Database.one

```ts
one<T = Row>(...statement: Statement): Promise<T | undefined>
```

The one row a statement gives, or undefined; two are InvalidError, and a write that gave them rolls back.

### Database.scalar

```ts
scalar<T = SqlValue>(...statement: Statement): Promise<T>
```

The one value of the one row a statement gives, as count(*) does; null as SQL answers it.

### Database.columns

```ts
columns<T = Row>(...statement: Statement): Promise<Columns<T>>
```

What `all` gives, by its columns: an array a column where `all` gives
an object a row, for rows by the thousand, a chart's points. It comes
faster, and a chart takes it as it is.

    const { ts, cpu } = await db.columns<Sample>`select ts, cpu from samples where host = ${host} order by ts`

### Database.each

```ts
each<T = Row>(...statement: Statement): AsyncGenerator<T>
```

The rows a statement gives, one at a time as their parts arrive: an
export that holds one part, not every row. The server reads them from
one snapshot, at most 64 MiB.

### Database.table

```ts
table<S extends StandardSchemaV1>(name: string, options: TableOptions & {
        schema: S
    }): Table<StandardSchemaV1.InferOutput<S>>
table<T = Row>(name: string, options?: TableOptions): Table<T>
```

A table of the database, typed where it opens: its rows' inserts and
upserts, and queries built without SQL text. It costs nothing until its
first call; open each once, in the module that owns it.

    const notes = db.table<Note>('notes', { types: { done: 'bool', created_at: 'time' } })

### Database.batch

```ts
batch(statements: readonly Sql[]): Promise<Done[]>
```

Statements known before they run, written as one: all of them or none, in a shared commit.

### Database.tx

```ts
tx<T>(fn: (tx: SqlTx) => T | Promise<T>): Promise<T>
```

Runs fn in a transaction that holds the writer, for writes that depend on
what its reads found: returning commits, a throw rolls back. It runs
once, five seconds at most, past which the server rolls it back; inside
it, call nothing that waits on the world.

    await db.tx(async tx => {
      const left = await tx.scalar<number>`select stock from items where sku = ${sku}`
      if (left < 1) throw new SoldOut()
      await tx.exec`update items set stock = stock - 1 where sku = ${sku}`
    })

### Database.rows

```ts
rows(statement: Sql, want: Want): Promise<Answered>
```

### Database.exec

```ts
exec(...statement: Statement): Promise<Done>
```

Runs a write once it is durable; writes from many callers share a commit, each failing alone.

## SqlTx

```ts
class SqlTx implements Runner {
    readonly describe: string
}
```

A database's transaction, from `db.tx`: its reads see its writes, and a
call that fails leaves it as it was before the call. Its calls go one at a
time, in order.

### SqlTx.with

```ts
with<T>(bucket: Bucket<T>): Bucket<T>
with<T>(queue: Queue<T>): TxQueue<T>
with<T>(channel: Channel<T>): TxChannel<T>
```

A bucket or a queue opened from this database, inside the transaction:
what it writes commits with the rows or not at all, and what it reads
sees what the transaction wrote.

    await db.tx(async tx => {
      const order = await tx.table<Order>('orders').insert({ user_id, total })
      await tx.with(emails).add({ order: order.id, to })
    })

### SqlTx.all

```ts
all<T = Row>(...statement: Statement): Promise<T[]>
```

### SqlTx.one

```ts
one<T = Row>(...statement: Statement): Promise<T | undefined>
```

### SqlTx.scalar

```ts
scalar<T = SqlValue>(...statement: Statement): Promise<T>
```

### SqlTx.exec

```ts
exec(...statement: Statement): Promise<Done>
```

### SqlTx.table

```ts
table<T = Row>(name: string, options?: TableOptions): Table<T>
```

A table inside the transaction: its calls are the transaction's.

### SqlTx.rows

```ts
rows(statement: Sql, want: Want): Promise<Answered>
```

### SqlTx.end

```ts
end(commit: boolean): Promise<void>
```

Sends the last call: commit, or roll back, and waits for the server's word.

## ColumnType

```ts
type ColumnType = 'bool' | 'time' | 'json' | 'bigint'
```

How a column SQLite keeps otherwise reads in JavaScript.

## TableOptions

```ts
interface TableOptions {
    /** the columns SQLite keeps as 0 or 1, unix milliseconds, JSON text or an integer past 2^53 */
    types?: Record<string, ColumnType>
    /** a Standard Schema, zod's or valibot's, that checks each row read */
    schema?: StandardSchemaV1
}
```

## UpsertOptions

```ts
interface UpsertOptions<T> {
    /** the columns whose value, taken, makes the insert an update: the key */
    conflict: string | readonly string[]
    /** the columns the update sets from the row; none other changes */
    update: readonly (keyof T & string)[]
    /** the condition the row that holds the key must meet, its owner's, or it stays as it is */
    where?: Condition | undefined
}
```

What an upsert does when the row's key is taken.

## RowPage

```ts
interface RowPage<T> {
    rows: T[]
    next?: string
}
```

A page of a query's rows, and the cursor that reads the next; none after the last.

## Query

```ts
class Query<T = Row> implements Embeddable {}
```

A query on a table: a value, each call returning a new one, so that the
first stays as it was. `all`, `one`, `scalar`, `count`, `each` and `list`
read; `update` and `delete` write.

    const urgent = await orders
      .where({ status: 'pending', deleted: false })
      .where(sql.or(sql`amount >= ${1000}`, { priority: 'high' }))
      .orderBy('created_at', 'desc')
      .limit(50)
      .all()

### Query.where

```ts
where(condition: Condition | string, ...values: unknown[]): Query<T>
```

Adds a condition with and: equal values, `null` as is null and a list
as in (…); a piece of SQL from `sql`; or text and the values of its ?s.
`undefined` is InvalidError, never a filter left out.

### Query.select

```ts
select<R = Row>(columns: string | Sql): Query<R>
```

The columns a row holds, as SQL text, and the shape of the row they make.

### Query.include

```ts
include<N extends string, C>(name: N, query: Query<C>): Query<T & Record<N, C[]>>
```

Gives each row the rows of another query as a list under `name`, from
the same statement: the query names the row it belongs to in its
`where`, by this table's alias, names its columns with `select`, and has
a `limit`, which counts for each row. Every value comes back as it is
kept, bytes and integers past 2^53 among them.

    const latest = db.table<Post>('posts as p').select<{ id: string; title: string }>('p.id, p.title')
      .where('p.author_id = u.id').orderBy('p.id', 'desc').limit(3)
    const authors = await db.table<User>('users as u').include('posts', latest).all()

### Query.join

```ts
join(table: string, on: string | Sql): Query<T>
```

Joins a table, `'users as u'`, on a condition as SQL text or a piece of SQL.

### Query.leftJoin

```ts
leftJoin(table: string, on: string | Sql): Query<T>
```

Joins a table whose columns may be null where no row of it meets the condition.

### Query.groupBy

```ts
groupBy(...columns: string[]): Query<T>
```

Groups the rows by columns, quoted as names.

### Query.having

```ts
having(condition: Condition | string, ...values: unknown[]): Query<T>
```

A condition on the groups, as `where` takes one.

### Query.orderBy

```ts
orderBy(column: string | Sql, direction?: Direction): Query<T>
```

Orders the rows by a column, quoted as a name, so that a sort a request
chooses cannot be SQL, or by a piece of SQL: `` orderBy(sql`lower(title)`) ``.

### Query.limit

```ts
limit(rows: number): Query<T>
```

### Query.offset

```ts
offset(rows: number): Query<T>
```

### Query.toSQL

```ts
toSQL(): {
        text: string
        values: SqlArg[]
    }
```

The text and the values the query sends, to read or to log.

### Query.embedded

```ts
embedded(): Sql
```

The query inside another piece of SQL: a subquery, in parentheses.

### Query.all

```ts
all(): Promise<T[]>
```

### Query.one

```ts
one(): Promise<T | undefined>
```

### Query.scalar

```ts
scalar<V = SqlValue>(): Promise<V>
```

### Query.count

```ts
count(): Promise<number>
```

Every row the conditions match, without the order and the limit.

### Query.each

```ts
each(): AsyncGenerator<T>
```

The rows one at a time, as their parts arrive.

### Query.list

```ts
list(options?: {
        limit?: number | undefined
        after?: string | undefined
    }): Promise<RowPage<T>>
```

A page after the last row of the one before, by the query's order,
which ends in a unique column so that no row is read twice or skipped.
`next` reads the next page, and only of this query.

    const first = await orders.orderBy('created_at', 'desc').orderBy('id', 'desc').list({ limit: 50 })
    const second = await orders.orderBy('created_at', 'desc').orderBy('id', 'desc').list({ limit: 50, after: first.next })

### Query.explain

```ts
explain(): Promise<string[]>
```

SQLite's plan for the query, a line a step.

### Query.update

```ts
update(values: {
        [K in keyof T]?: T[K] | Sql
    } & Record<string, unknown>): Promise<Done>
```

Sets columns of the rows the conditions match, and says how many changed.

### Query.delete

```ts
delete(): Promise<Done>
```

Deletes the rows the conditions match, and says how many it deleted.

## Table

```ts
class Table<T = Row> extends Query<T> {
    readonly name: string
}
```

A table: its queries, and its rows' inserts and upserts.

    const notes = db.table<Note>('notes', { types: { done: 'bool', tags: 'json', created_at: 'time' } })
    const note = await notes.insert({ id: uuidv7(), author_id: userId, title: 'Buy milk', done: false, tags: [], created_at: new Date() })

### Table.insert

```ts
insert(row: Partial<T>): Promise<T>
insert(rows: readonly Partial<T>[]): Promise<T[]>
```

Inserts a row and gives it back as the file keeps it; a list of rows is one write, all of them or none.

### Table.upsert

```ts
upsert(row: Partial<T>, options: UpsertOptions<T>): Promise<T | undefined>
```

Inserts a row, or, when its key is taken, sets the columns `update`
names on the row that holds it, if that row meets `where`. It gives
back the row as the file keeps it, or undefined when `where` kept it.

<!-- Generated by task reference from sdk/js/src/sql.ts, sdk/js/src/query.ts. Edit the doc comments there, not this file. -->
