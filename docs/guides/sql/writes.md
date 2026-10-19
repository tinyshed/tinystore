# Writes and transactions

A database has three ways to write, and they cost different things. `exec`
runs one statement and shares its commit with whatever else arrives at the
same moment. `batch` runs a list of statements known in advance as one, in
the same shared commit. `tx` runs your function while it holds the writer
alone, for writes that depend on what it reads.

| Call    | Use it for                             | Commit                      | Fails                |
|---------|----------------------------------------|-----------------------------|----------------------|
| `exec`  | one statement                          | shared with other writes    | alone                |
| `batch` | statements known before they run       | shared, all of them or none | all of them together |
| `tx`    | writes that depend on what reads found | its own                     | all of them together |

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
  holds.

## When a write fails

| Error             | When                                                                | Do                                       |
|-------------------|---------------------------------------------------------------------|------------------------------------------|
| `conflict`        | a unique or a primary key is taken                                  | read the row that holds it               |
| `invalid`         | a foreign key, a `check`, a `not null`, a value of the wrong type   | fix the value; `constraint` says which   |
| `limit`           | a transaction past five seconds; `all` past 64 MiB of rows          | do less; read with `each` or `list`      |
| `outcome unknown` | the commit's sync failed, or the connection ended before the answer | read what you wrote before writing again |

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
