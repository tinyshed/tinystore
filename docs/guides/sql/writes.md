# Writes and transactions

A database has three ways to write, and they cost different things. `exec`
runs one statement and shares its commit with whatever else arrives at the
same moment. `batch` runs a list of writes known in advance as one, in the
same shared commit. A batch takes statements, and jobs for a queue that the
database keeps. `tx` runs your function while it holds the writer alone, for
writes that depend on what it reads.

| Call    | Use it for                                | Commit                      | Fails                |
|---------|-------------------------------------------|-----------------------------|----------------------|
| `exec`  | one statement                             | shared with other writes    | alone                |
| `batch` | statements and jobs known before they run | shared, all of them or none | all of them together |
| `tx`    | writes that depend on what reads found    | its own                     | all of them together |

## One statement

```ts
const done = await db.exec`insert into orders (user_id, total, status, created_at)
	values (${userId}, ${total}, 'pending', ${Date.now()})`
done.lastInsertRowid // the new order's id
done.changes // 1
```

```python
done = await db.exec(
    "insert into orders (user_id, total, status, created_at) values (?, ?, 'pending', ?)",
    user_id, total, time.time_ns() // 1_000_000,
)
done.last_insert_rowid  # the new order's id
```

```go
done, err := db.Exec(ctx, `insert into orders (user_id, total, status, created_at) values (?, ?, 'pending', ?)`,
	userID, total, time.Now().UnixMilli())
done.LastInsertRowid // the new order's id
```

```rust
let done = db.exec(sql!(
    "insert into orders (user_id, total, status, created_at) values (?, ?, 'pending', ?)",
    user_id,
    total,
    unix_millis(SystemTime::now())
))?;
done.last_insert_rowid // the new order's id
```

`exec` returns once the write is on the disk. A value is always a parameter,
never SQL: TypeScript's `${…}` in the `sql` template, a `?` in Python and Go,
an argument of Rust's `sql!`.

## Rows back from a write

A write with `returning` goes through a read's verb, `one`, `all` or
`scalar`. It runs on the writer and answers once its commit is durable
(contract SQL-01):

```ts
const order = await db.one<{ id: number; created_at: number }>`
	insert into orders (user_id, total, status, created_at) values (${userId}, ${total}, 'pending', ${Date.now()})
	returning id, created_at`
```

```python
order = await db.one(
    "insert into orders (user_id, total, status, created_at) values (?, ?, 'pending', ?) returning id, created_at",
    user_id, total, time.time_ns() // 1_000_000,
)
```

```go
id, err := sqldb.Scalar[int64](ctx, db,
	`insert into orders (user_id, total, status, created_at) values (?, ?, 'pending', ?) returning id`,
	userID, total, time.Now().UnixMilli())
```

```rust
let id: i64 = db.scalar(sql!(
    "insert into orders (user_id, total, status, created_at) values (?, ?, 'pending', ?) returning id",
    user_id,
    total,
    unix_millis(SystemTime::now())
))?;
```

## Statements known in advance

```ts
await db.batch([
	sql`update accounts set balance = balance - ${amount} where id = ${from}`,
	sql`update accounts set balance = balance + ${amount} where id = ${to}`,
	sql`insert into transfers (from_id, to_id, amount) values (${from}, ${to}, ${amount})`,
])
```

```python
await db.batch([
    sql("update accounts set balance = balance - ? where id = ?", amount, from_id),
    sql("update accounts set balance = balance + ? where id = ?", amount, to_id),
    sql("insert into transfers (from_id, to_id, amount) values (?, ?, ?)", from_id, to_id, amount),
])
```

```go
err = db.Batch(ctx,
	sqldb.SQL(`update accounts set balance = balance - ? where id = ?`, amount, fromID),
	sqldb.SQL(`update accounts set balance = balance + ? where id = ?`, amount, toID),
	sqldb.SQL(`insert into transfers (from_id, to_id, amount) values (?, ?, ?)`, fromID, toID, amount),
)
```

```rust
db.batch([
    sql!("update accounts set balance = balance - ? where id = ?", amount, from),
    sql!("update accounts set balance = balance + ? where id = ?", amount, to),
    sql!("insert into transfers (from_id, to_id, amount) values (?, ?, ?)", from, to, amount),
])?;
```

All three statements are written, or none. A batch is one savepoint of the
next shared commit, so it costs what one write does. Prefer it to a
transaction whenever the statements do not depend on a read: a `check
(balance >= 0)` on the table refuses an overdraft without one.

## A row and its job in one batch

A queue that you open from the database, `db.queue(…)`, keeps its jobs in the
database's file. Its `adding` gives a job that a batch writes together with
its statements:

```ts
const indexing = db.queue<{ noteId: number }>('indexing')

await db.batch([
	sql`update notes set body = ${body} where id = ${noteId}`,
	indexing.adding({ noteId }), // written with the row, or not at all
])
```

```python
indexing = db.queue("indexing", Index)

await db.batch([
    sql("update notes set body = ? where id = ?", body, note_id),
    indexing.adding(Index(note_id=note_id)),  # written with the row, or not at all
])
```

```go
indexing, err := jobs.OpenQueue[Index](db, "indexing")

err = db.Batch(ctx,
	sqldb.SQL(`update notes set body = ? where id = ?`, body, noteID),
	indexing.Adding(Index{NoteID: noteID}), // written with the row, or not at all
)
```

```rust
let indexing = db.queue::<Index>("indexing").open()?;

db.batch([
    sql!("update notes set body = ? where id = ?", body, note_id).into(),
    indexing.adding(&Index { note_id }), // written with the row, or not at all
])?;
```

- The row and the job are both written, or neither, and a worker sees the
  job only after the commit (contract JOB-02).
- `adding` accepts the same options as `add`: an id, a time, a group. If the
  id is taken, the job is not added, and its entry in the batch's answer has
  `changes` set to 0 instead of 1.
- A queue of the store, `store.queue(…)`, lives in `jobs.db`. A batch of the
  database refuses its jobs with an invalid error, because no write is atomic
  across two files.

Pick the write by what it costs:

| Write                                      | Atomic | Shares its commit with other writes |
|--------------------------------------------|--------|-------------------------------------|
| `exec`, then `add` on a queue of the store | no     | yes                                 |
| `tx` with `tx.with(queue).add`             | yes    | no                                  |
| `batch` with `queue.adding`                | yes    | yes                                 |

A transaction holds the writer and pays for its own sync. So `db.tx` with
`tx.with(queue)` costs one sync per call, and it never groups with other
writes. Two separate commits do group, but a crash between them keeps the row
and loses the job. Use a batch when the job does not depend on what a read
finds.

## A transaction

A transaction is for writes that depend on what it reads: take the last
seat, or refuse.

```ts
const booked = await db.tx(async tx => {
	const left = await tx.scalar<number>`select seats_left from flights where id = ${flightId}`
	if (left === 0) return false
	await tx.exec`update flights set seats_left = seats_left - 1 where id = ${flightId}`
	await tx.exec`insert into bookings (flight_id, user_id) values (${flightId}, ${userId})`
	return true
})
```

```python
async def book(tx: tinystore.SqlTx) -> bool:
    left = await tx.scalar(int, "select seats_left from flights where id = ?", flight_id)
    if left == 0:
        return False
    await tx.exec("update flights set seats_left = seats_left - 1 where id = ?", flight_id)
    await tx.exec("insert into bookings (flight_id, user_id) values (?, ?)", flight_id, user_id)
    return True


booked = await db.tx(book)
```

```go
var booked bool
err = db.Tx(ctx, func(tx *sqldb.Tx) error {
	left, err := sqldb.Scalar[int64](ctx, tx, `select seats_left from flights where id = ?`, flightID)
	if err != nil || left == 0 {
		return err
	}
	if _, err := tx.Exec(ctx, `update flights set seats_left = seats_left - 1 where id = ?`, flightID); err != nil {
		return err
	}
	_, err = tx.Exec(ctx, `insert into bookings (flight_id, user_id) values (?, ?)`, flightID, userID)
	booked = err == nil
	return err
})
```

```rust
let booked = db.tx(|tx| -> tinystore::Result<bool> {
    let left: i64 = tx.scalar(sql!("select seats_left from flights where id = ?", flight_id))?;
    if left == 0 {
        return Ok(false);
    }
    tx.exec(sql!("update flights set seats_left = seats_left - 1 where id = ?", flight_id))?;
    tx.exec(sql!("insert into bookings (flight_id, user_id) values (?, ?)", flight_id, user_id))?;
    Ok(true)
})?;
```

- Returning commits; throwing, or returning an error, rolls back (contract
  SQL-07).
- Its reads see its own writes. A call that fails leaves the transaction as
  it was before that call, so the function may catch an error and go on.
  The exception is a statement that rolls back the whole transaction, such
  as `insert or rollback`, a column's `on conflict rollback`, or a trigger's
  `raise(rollback)`. It ends the transaction. Every call after it fails
  `invalid`, the commit fails too, and nothing of the transaction is written.
- It runs once. TinyStore never runs it again on a conflict, because a
  function that called a payment API would charge twice.
- It holds the writer: every other write to this file waits. Past five
  seconds it is rolled back and fails `limit`. Call nothing that waits on the
  world inside it, no model, no HTTP.
- A transaction's tables are `tx.table(…)`; a bucket, a queue or a channel
  joins through `tx.with(…)` (`with_` in Python, `.In(tx)` in Go). See
  [Buckets and queues in a database](buckets-and-queues.md).
- A call on the database itself, not on `tx`, from inside its own
  transaction is `invalid`: it would wait for the writer the transaction
  holds. In Rust, so is `store.close()`. Close the store after the
  transaction.
- In Rust, a write to another file from inside the function, such as a
  bucket of the store, waits for that file's writer. If a transaction that
  holds that writer is waiting for this one's, the write fails `unavailable`
  instead of waiting forever. Return the error so that the transaction rolls
  back, and run it again.

## When a write fails

| Error             | When                                                                                                           | Do                                                           |
|-------------------|----------------------------------------------------------------------------------------------------------------|--------------------------------------------------------------|
| `conflict`        | a unique or a primary key is taken                                                                             | read the row that holds it                                   |
| `invalid`         | a foreign key, a `check`, a `not null`, a value of the wrong type; a syntax error, a table or column not there | fix the value or the statement; `constraint` says which rule |
| `limit`           | a transaction past five seconds; `all` past 64 MiB of rows                                                     | do less; read with `each` or `list`                          |
| `unavailable`     | another write in the same shared commit rolled the whole commit back                                           | run the same write again                                     |
| `outcome unknown` | the commit's sync failed, or the connection ended before the answer                                            | read what you wrote before writing again                     |

A statement that rolls back its whole transaction, such as `insert or
rollback` on a taken key, also rolls back the commit that `exec` and `batch`
share with other writes. It fails with its own error. The writes that ran
before it in that commit fail `unavailable` and wrote nothing, so they can run
again. The writes after it commit in the next transaction.

A broken constraint names itself (contract SQL-08):

```ts
try {
	await users.insert({ email })
} catch (err) {
	if (err instanceof ConflictError && err.constraint?.columns.includes('email')) return taken()
	throw err
}
```

```python
try:
    await users.insert({"email": email})
except tinystore.ConflictError as err:
    if err.constraint and "email" in err.constraint.columns:
        return taken()
    raise
```

```go
_, err = users.Insert(ctx, User{Email: email})
var e *tinystore.Error
if errors.As(err, &e) && e.Kind == tinystore.Conflict && e.Constraint != nil && slices.Contains(e.Constraint.Columns, "email") {
	return taken()
}
```

```rust
match users.insert(&User { email, ..Default::default() }) {
    Err(err) if err.constraint().is_some_and(|c| c.columns.iter().any(|c| c == "email")) => return Ok(taken()),
    other => other?,
};
```

The constraint's `kind` is `unique`, `primaryKey`, `foreignKey`, `check` or
`notNull`, with its `table`, its `columns`, or a check's `name`.

## Values

A value comes back as it went in, and a value SQLite would change is refused
(contract SQL-05):

| Your value            | Kept as                                                    |
|-----------------------|------------------------------------------------------------|
| a whole number        | `integer`, whole to 2⁶³; a `bigint` in TypeScript past 2⁵³ |
| a fractional number   | `real`; NaN is refused                                     |
| text                  | `text`                                                     |
| bytes                 | `blob`                                                     |
| `true` and `false`    | `1` and `0`                                                |
| `null`, `None`, `nil` | `NULL`                                                     |
| an object, a list     | refused: wrap it in `sql.json(…)` to keep its JSON         |

A `?` without its value is refused, not read as `NULL`.

## Durability

A database commits as the store does, `full` unless the store says
otherwise. A database of what can be rebuilt, a cache of rendered pages, may
commit faster:

```ts
const cache = await store.database('cache', { durability: 'os' })
```

```python
cache = await store.database("cache", durability="os")
```

```go
cache, err := sqldb.Open(ctx, store, "cache", sqldb.Durability("os"))
```

```rust
let cache = store.database("cache").durability(Durability::Os).open()?;
```

With `os`, a write is in the operating system's hands when it returns: a
crash of the program loses nothing, a power cut may lose the last writes.

## See also

- [SQL](README.md): the engine's overview.
- [Tables and queries](tables.md): inserts, updates and deletes without SQL
  text.
- [A write's journey](../../handbook/inside/journey-of-a-write.md) in the
  Handbook: how a shared commit is built.
