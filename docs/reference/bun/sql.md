# SQL API for Bun and Node

Every public class, function and type of the SQL engine in `@tinyshed/tinystore`, generated from its source. The [SQL guide](../../sql/README.md) explains how to use them, and the [Python](../python/sql.md) and [Go](../go/sql.md) pages list the same API.

## Row

```ts
type Row = Record<string, SqlValue>
```

A row as SQLite returned it, by its columns' names.

## Migrations

```ts
type Migrations = string | Record<string, string> | readonly {
    name: string
    text: string
}[]
```

A database's migrations: a directory whose .sql files they are, at its root
or in its one directory, or the files themselves by name.

## SqlOptions

```ts
interface SqlOptions {
    /** what the first open applies, on an admin connection, and every later open checks */
    migrations?: Migrations
}
```

## Done

```ts
interface Done {
    changes: number
    lastId: number
}
```

What a write changed: the rows, and the rowid of the last it inserted.

## Statement

```ts
type Statement = [sql: string | TemplateStringsArray, ...args: unknown[]]
```

A statement's text and arguments, in any of the three ways a call takes
them: positional after the text, one object of named ones, or a template
whose values are its arguments.

    db.all('select * from notes where id = ?', id)
    db.all('select * from notes where id = :id', { id })
    db.all`select * from notes where id = ${id}`

## Database

```ts
class Database {
    readonly name: string
}
```

A database. What a call's name starts with says where it runs: exec and
its kin may write and run on the file's one writer, grouped with the other
writes; all, one, scalar and each read, on readers that refuse to write.

### Database.handle

```ts
handle(connection: Connection): Promise<number>
```

### Database.all

```ts
all<T = Row>(...statement: Statement): Promise<T[]>
```

Every row a query answers, held in memory: the server refuses past 64 MiB of them.

### Database.one

```ts
one<T = Row>(...statement: Statement): Promise<T | undefined>
```

The one row a query answers, undefined for none; two is InvalidError.

### Database.scalar

```ts
scalar<T = SqlValue>(...statement: Statement): Promise<T>
```

The one value of a query that always answers one row, as count(*) does.

### Database.query

```ts
query(...statement: Statement): Promise<{
        columns: string[]
        rows: SqlValue[][]
    }>
```

The columns and rows of a query as SQLite returned them, arrays rather than objects.

### Database.each

```ts
each<T = Row>(...statement: Statement): AsyncGenerator<T>
```

Reads a query's rows one at a time as they arrive, within the credit
the client grants: the client holds a row and the frames in flight.

### Database.exec

```ts
exec(...statement: Statement): Promise<Done>
```

Runs a statement on the writer and says what it changed.

### Database.execAll

```ts
execAll<T = Row>(...statement: Statement): Promise<T[]>
```

The rows a write's returning clause answers, run on the writer.

### Database.execOne

```ts
execOne<T = Row>(...statement: Statement): Promise<T | undefined>
```

### Database.execScalar

```ts
execScalar<T = SqlValue>(...statement: Statement): Promise<T>
```

### Database.batch

```ts
batch<const T = void>(fn: (tx: SqlBatch) => T): Promise<Settled<T>>
```

Runs the statements fn asks for in one transaction, all or none: a
failure names its statement as `call`. fn returns before anything is
sent, since no transaction is held across the network, and what it
returns comes back answered, an array's promises each:

    const [note] = await db.batch(tx => [tx.one`insert into notes (title) values (${t}) returning *`])

### Database.view

```ts
view<const T = void>(fn: (tx: SqlBatch) => T): Promise<Settled<T>>
```

Runs the reads fn asks for from one snapshot, and gives back what fn
returns, answered as batch answers it:

    const [notes, total] = await db.view(tx => [tx.all`select * from notes`, tx.scalar`select count(*) from notes`])

## SqlBatch

```ts
class SqlBatch {
    readonly read: boolean
    readonly database: string
    readonly statements: Batched[]
    readonly jobs: Queued[]
    readonly keys: Keyed[]
}
```

The statements of a batch or a view, whose promises settle with it.

### SqlBatch.close

```ts
close(): void
```

### SqlBatch.made

```ts
made(promise: Promise<unknown>): boolean
```

Whether the promise is one of this batch's statements', rather than an async function's.

### SqlBatch.enqueue

```ts
enqueue(open: Uint8Array, job: Queued['job']): Promise<void>
```

Adds a job to the batch, for a queue's withTx: a program writes
`queue.withTx(tx).enqueue(value)`, on a queue opened `in` this database.

### SqlBatch.writeKey

```ts
writeKey(open: Uint8Array, operation: Keyed['operation']): Promise<void>
```

Adds a key's write to the batch, for a bucket's withTx: a program writes
`bucket.withTx(tx).set(key, value)`, on a bucket opened `in` this database.

### SqlBatch.exec

```ts
exec(...statement: Statement): Promise<Done>
```

### SqlBatch.all

```ts
all<T = Row>(...statement: Statement): Promise<T[]>
```

### SqlBatch.one

```ts
one<T = Row>(...statement: Statement): Promise<T | undefined>
```

### SqlBatch.scalar

```ts
scalar<T = SqlValue>(...statement: Statement): Promise<T>
```

<!-- Generated by task reference from sdk/js/src/sql.ts. Edit the doc comments there, not this file. -->
