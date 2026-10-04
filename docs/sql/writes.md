# Writes and transactions

Every SQL write returns after it is saved to disk, and writes from many
requests share one disk sync. Use a batch when several statements must
succeed or fail together, and in Go, a transaction when you need to read
before you decide what to write.

## Writes are grouped

```ts
await Promise.all(messages.map(m =>
	db.exec`insert into messages (id, chat, text) values (${m.id}, ${m.chat}, ${m.text})`,
))
```

```python
await asyncio.gather(*(
    db.exec("insert into messages (id, chat, text) values (?, ?, ?)", m.id, m.chat, m.text)
    for m in messages
))
```

```go
for _, m := range messages {
	go func() {
		_, err := db.Exec(ctx, `insert into messages (id, chat, text) values (?, ?, ?)`, m.ID, m.Chat, m.Text)
		report(err)
	}()
}
```

SQLite has one writer per file. TinyStore queues writes that arrive at the
same time and commits them together, each in a savepoint of one transaction,
with one disk sync. If one write fails, for example on a unique constraint,
only that write is rolled back, and the others are saved.

With 512 concurrent callers, grouped inserts reached about 18,000 per second,
where `database/sql` with one transaction per insert managed about 390, and
failed some of them with `database is locked`. Measured on Windows 11, Ryzen 7
7700, NVMe
([report](https://github.com/tinyshed/research/blob/main/tinystore/reports/sqldb-engine-2026-09-28.md)).

## Several statements as one

```ts
await db.batch(tx => {
	tx.exec`update accounts set balance = balance - ${amount} where id = ${from}`
	tx.exec`update accounts set balance = balance + ${amount} where id = ${to}`
	tx.exec`insert into transfers (from_id, to_id, amount) values (${from}, ${to}, ${amount})`
})
```

```python
async with db.batch() as tx:
    tx.exec("update accounts set balance = balance - ? where id = ?", amount, from_id)
    tx.exec("update accounts set balance = balance + ? where id = ?", amount, to_id)
    tx.exec("insert into transfers (from_id, to_id, amount) values (?, ?, ?)", from_id, to_id, amount)
```

```go
err = db.Batch(ctx, func(b *sqldb.Batch) error {
	b.Exec(`update accounts set balance = balance - ? where id = ?`, amount, from)
	b.Exec(`update accounts set balance = balance + ? where id = ?`, amount, to)
	b.Exec(`insert into transfers (from_id, to_id, amount) values (?, ?, ?)`, from, to, amount)
	return nil
})
```

A batch is built before it runs, and then committed in one savepoint of the
next group: all of its statements, or none of them. Because your code doesn't
run while the batch holds the writer, a batch is as cheap as a single write.

A statement that needs the result of an earlier one says so in SQL, with
`last_insert_rowid()` or a key your program chose. To check a balance before
you change it, put the check into the SQL itself, for example
`where id = ? and balance >= ?`, and look at the number of changed rows.

## Read, then write (Go)

```go
err = db.Tx(ctx, func(tx *sqldb.Tx) error {
	balance, err := sqldb.Scalar[int64](ctx, tx, `select balance from accounts where id = ?`, from)
	if err != nil {
		return err
	}
	if balance < amount {
		return errInsufficientFunds
	}
	if _, err = tx.Exec(ctx, `update accounts set balance = balance - ? where id = ?`, amount, from); err != nil {
		return err
	}
	_, err = tx.Exec(ctx, `update accounts set balance = balance + ? where id = ?`, amount, to)
	return err
})
```

`Tx` runs your function in a transaction of its own: returning nil commits,
returning an error or panicking rolls back. A transaction holds the writer
while your code runs and pays for its own disk sync, so transactions from many
goroutines run one at a time. Use a batch whenever you can.

Bun and Python have no transaction that stays open across calls, so that a
slow client can never block the writer. Use a batch, with conditions in the
SQL.

## Read from one moment

```ts
const [page, total] = await db.view(tx => [
	tx.all<Note>`select * from notes order by id limit 20`,
	tx.scalar<number>`select count(*) from notes`,
])
```

```python
async with db.view() as tx:
    page = tx.all(Note, "select * from notes order by id limit 20")
    total = tx.scalar("select count(*) from notes")
print(await page, await total)
```

```go
err = db.View(ctx, func(tx *sqldb.Tx) error {
	var err error
	if page, err = sqldb.All[Note](ctx, tx, `select * from notes order by id limit 20`); err != nil {
		return err
	}
	total, err = sqldb.Scalar[int](ctx, tx, `select count(*) from notes`)
	return err
})
```

A view runs its reads from one snapshot, so the page and the total agree. A
view can be held for at most five seconds, because a long snapshot makes the
database's write-ahead log grow.

## Constraint errors

```ts
try {
	await db.exec`insert into users (email) values (${email})`
} catch (err) {
	if (err instanceof ConflictError) return emailTaken()
	throw err
}
```

```python
try:
    await db.exec("insert into users (email) values (?)", email)
except ConflictError:
    return email_taken()
```

```go
_, err = db.Exec(ctx, `insert into users (email) values (?)`, email)
if c, ok := errors.AsType[*sqldb.ConstraintError](err); ok && c.Kind == sqldb.UniqueViolation {
	return errEmailTaken
}
```

A unique or primary key violation is a conflict error. Other constraints, such
as a check or a foreign key, are invalid argument errors. In Go,
`*sqldb.ConstraintError` also tells you its kind, `UniqueViolation`,
`PrimaryKeyViolation`, `ForeignKeyViolation`, `CheckViolation` or
`NotNullViolation`, and the table and constraint when SQLite names them.

## When the outcome is unknown

If the disk sync of a group fails, TinyStore can't know whether the group was
saved. Each write of the group then fails with an outcome unknown error
(`ErrOutcomeUnknown` in Go, `OutcomeUnknownError` in Bun and Python). Read
what you wrote before you try again.

## Long exports

`each` reads rows one at a time, so your program holds one row at a time
instead of the whole answer.

```ts
for await (const note of db.each<Note>`select * from notes order by id`) {
	await out.write(note)
}
```

```python
async for note in db.each(Note, "select * from notes order by id"):
    await out.write(note)
```

```go
for note, err := range sqldb.Each[Note](ctx, db, `select * from notes order by id`) {
	if err != nil {
		return err
	}
	out.Write(note)
}
```

In Go, `each` reads from one snapshot, which lasts at most five seconds. In
Bun and Python, the server reads the rows from one snapshot and holds at most
64 MiB of them before it sends the first. For a longer or larger export, read
pages by key, `where id > ? order by id limit 1000`, and write `limit cast(? as
integer)` instead of `limit ?`, so that SQLite doesn't compile the statement
again for every page.

## Limits and defaults

|                              |                  |
|------------------------------|------------------|
| Writes committed together    | 1,024, or 8 MiB  |
| A group's hold on the writer | 10 seconds       |
| A view or `each` snapshot    | 5 seconds        |
| Rows `all` returns           | 64 MiB of values |

## See also

- [Jobs and your data](../jobs/your-data.md): commit a job in the same batch
  as your rows.
- [sqldb/README.md](../../sqldb/README.md): the full contract of writes.
