# Jobs and your data

A job and a row of your SQL database are often about the same thing: a new
note and the job that indexes it, an account and the job that deletes it in
30 days. This page shows two ways to keep them consistent: commit the job in
the same transaction as the row, or let the row be the truth and the job only
its alarm.

## Commit a job with your rows

```ts
const db = await store.sql('app', { migrations: './migrations' })
const index = store.jobs.queue<IndexNote>('index', { in: db }) // the queue lives in sql/app.db

await db.batch(tx => {
	tx.exec`update notes set body = ${body} where id = ${id}`
	index.withTx(tx).enqueue({ id })
})
```

```python
db = await store.sql("app", migrations="./migrations")
index = store.jobs.queue("index", IndexNote, in_=db)  # the queue lives in sql/app.db

async with db.batch() as tx:
    tx.exec("update notes set body = ? where id = ?", body, note_id)
    index.with_tx(tx).enqueue(IndexNote(id=note_id))
```

```go
db, err := sqldb.Open(ctx, store, "app", migrations, schema)
queues, err := jobs.Open(ctx, store, jobs.Options{In: db}) // the queues live in sql/app.db
index, err := jobs.OpenQueue[IndexNote](ctx, queues, "index")

err = db.Batch(ctx, func(b *sqldb.Batch) error {
	b.Exec(`update notes set body = ? where id = ?`, body, id)
	b.Add(index.Enqueued(ctx, IndexNote{ID: id}))
	return nil
})
```

By default, jobs live in their own file, `jobs.db`, so a job and a row are two
separate commits. With the `in` option (`In` in Go, `in_` in Python), a queue
lives inside your SQL database instead, in tables named `_tinystore_jobs…`.
The batch then commits the update and the job together: either both are
saved, or neither. If the update fails, for example because of a constraint,
the job isn't enqueued.

`withTx` (`with_tx` in Python) accepts only a batch of the database that the
queue lives in. If the queue lives in `jobs.db`, or if the batch is a view,
`withTx` fails with an invalid error (`ErrInvalid`, `InvalidError`). Outside a
batch, the queue works like any other queue, with `enqueue`, `work` and the
rest.

In Go, this is also the fastest way to write both. In a benchmark with 64
clients, an application with jobs in its SQL file served 1.49 times as many
requests per second as the same application on Redis, PostgreSQL,
VictoriaMetrics and files, while the default with separate files served about
as many as that stack (Linux container, Ryzen 7 7700, Go 1.27.1,
[report](https://github.com/tinyshed/research/blob/main/tinystore/reports/runtime-continuation-2026-10-01.md)).

In Bun and Python, the server runs a batch as one transaction with its own
disk sync. So a batch isn't faster than two separate writes there, but it is
atomic.

The jobs store opens after the database and closes before it.

## Read before you enqueue (Go)

```go
job := index.Enqueued(ctx, IndexNote{ID: id}) // before the transaction starts
err = db.Tx(ctx, func(tx *sqldb.Tx) error {
	if _, err := tx.Exec(ctx, `update notes set body = ? where id = ?`, body, id); err != nil {
		return err
	}
	return tx.Add(ctx, job) // commits with the update, or not at all
})
```

A Go transaction can read before it writes, and `tx.Add` adds a job to it.
Create the job with `Enqueued` before the transaction starts. Creating a job
can wait for memory, and a transaction must not wait while it holds the
database's writer.

Bun and Python have no SQL transaction that stays open while your code reads.
Put the condition in the SQL of the batch instead, for example
`update notes set body = ? where id = ? and version = ?`.

## When the job is the data

Some things exist only until they happen: a scheduled message, a reminder, a
push notification. Keep them only in the queue, and let the handler write the
result into your database, keyed by the job's own id:

```ts
await later.work(async job => {
	const d = job.value
	await db.exec`insert into messages (id, chat, text, sent_at)
		values (${d.id}, ${d.chat}, ${d.text}, ${Date.now()}) on conflict (id) do nothing`
})
```

```python
async def send(job: tinystore.Job[Draft]) -> None:
    d = job.value
    await db.exec(
        "insert into messages (id, chat, text, sent_at) values (?, ?, ?, ?) on conflict (id) do nothing",
        d.id, d.chat, d.text, int(time.time() * 1000),
    )


await later.work(send)
```

```go
err = later.Work(ctx, func(ctx context.Context, job jobs.Job[Draft]) error {
	d := job.Value
	_, err := db.Exec(ctx, `insert into messages (id, chat, text, sent_at)
		values (?, ?, ?, ?) on conflict (id) do nothing`, d.ID, d.Chat, d.Text, store.Now())
	return err
})
```

If the handler runs twice after a crash, `on conflict do nothing` inserts the
message once. The queue is as durable as your database: the same SQLite, the
same disk syncs, the same backup.

## When the row is the truth

Other things are part of your data: an account that will be deleted in 30
days, a trial that ends, an invoice reminder. The settings page reads the row,
and "cancel" changes the row. Here the job is only an alarm that wakes your
program at the right time. The handler reads the row and does what the row
says:

```ts
await deletions.enqueue(userId, { at: deleteAt, key: `user:${userId}` }) // the alarm first
await db.exec`update users set delete_at = ${deleteAt.getTime()} where id = ${userId}`

await deletions.work(async job => {
	const user = await db.one<{ delete_at: number | null }>`select delete_at from users where id = ${job.value}`
	if (!user?.delete_at) return                          // cancelled, or already deleted
	if (user.delete_at > Date.now()) {
		return job.snooze({ at: new Date(user.delete_at) }) // the row says later
	}
	await purgeUser(job.value)
})
```

```python
await deletions.enqueue(user_id, at=delete_at, key=f"user:{user_id}")  # the alarm first
await db.exec("update users set delete_at = ? where id = ?", int(delete_at.timestamp() * 1000), user_id)


async def delete_account(job: tinystore.Job[int]) -> None:
    user = await db.one("select delete_at from users where id = ?", job.value)
    if user is None or user["delete_at"] is None:
        return  # cancelled, or already deleted
    if user["delete_at"] > time.time() * 1000:
        return job.snooze(at=datetime.fromtimestamp(user["delete_at"] / 1000, UTC))  # the row says later
    await purge_user(job.value)


await deletions.work(delete_account)
```

```go
err = deletions.Enqueue(ctx, userID, jobs.At(deleteAt), jobs.Key(fmt.Sprint("user:", userID))) // the alarm first
_, err = db.Exec(ctx, `update users set delete_at = ? where id = ?`, deleteAt, userID)

err = deletions.Work(ctx, func(ctx context.Context, job jobs.Job[int64]) error {
	at, pending, err := deletionTime(ctx, job.Value)
	switch {
	case err != nil:
		return err
	case !pending:
		return nil // cancelled, or already deleted
	case at.After(store.Now()):
		return job.Snooze(ctx, jobs.At(at)) // the row says later
	}
	return purgeUser(ctx, job.Value)
})
```

Every crash leaves a consistent state:

- If the program crashes between the two writes, the alarm finds no
  `delete_at` and does nothing.
- If the user cancels, the row is cleared, and the alarm does nothing.
- If the row moves later, the alarm wakes early and snoozes until the new
  time. A snooze doesn't count as an attempt.
- If the row moves earlier, enqueue the key again. A key's job can only move
  earlier, so the alarm moves with the row.

Neither pattern polls the database, and neither needs a job that repairs what
a crash left behind.

## See also

- [Keys](keys.md): how enqueuing a key again moves a job.
- [SQL: writes and transactions](../sql/writes.md): batches in your database.
- [jobs/README.md](../../jobs/README.md): the full contract of `In`.
