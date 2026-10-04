# KV and your data

A key and a row of your SQL database are often about the same thing: a user
and their session, an order and its cached total. With the `in` option, a KV
bucket lives inside your SQL database, and a batch commits its keys together
with your rows: either both are saved, or neither.

## Commit a key with your rows

```ts
const db = await store.sql('app', { migrations: './migrations' })
const sessions = store.kv.bucket<Session>('sessions', { in: db }) // the bucket lives in sql/app.db

await db.batch(tx => {
	tx.exec`insert into users (id, email) values (${id}, ${email})`
	sessions.withTx(tx).set(token, { user: id }, { ttl: '30d' })
})
```

```python
db = await store.sql("app", migrations="./migrations")
sessions = store.kv.bucket("sessions", Session, in_=db)  # the bucket lives in sql/app.db

async with db.batch() as tx:
    tx.exec("insert into users (id, email) values (?, ?)", user_id, email)
    sessions.with_tx(tx).set(token, Session(user=user_id), ttl="30d")
```

```go
db, err := sqldb.Open(ctx, store, "app", migrations, schema)
state, err := kv.Open(ctx, store, kv.Options{In: db}) // the buckets live in sql/app.db
sessions, err := kv.OpenBucket[Session](ctx, state, "sessions")

err = db.Batch(ctx, func(b *sqldb.Batch) error {
	b.Exec(`insert into users (id, email) values (?, ?)`, id, email)
	b.Add(sessions.Written(ctx, token, Session{User: id}, kv.TTL(30*24*time.Hour)))
	return nil
})
```

By default, KV keeps its buckets in its own file, `kv.db`, so a key and a row
are two separate commits. With `in` (`In` in Go, `in_` in Python), the buckets
live inside your SQL database instead, in tables named `_tinystore_kv…`. If
the insert fails, for example because the email is already taken, the session
isn't written either.

Inside a batch, a bucket can set, delete and clear keys:

| Bun                                 | Python                               | Go                                       |
|-------------------------------------|--------------------------------------|------------------------------------------|
| `bucket.withTx(tx).set(key, value)` | `bucket.with_tx(tx).set(key, value)` | `b.Add(bucket.Written(ctx, key, value))` |
| `bucket.withTx(tx).delete(key)`     | `bucket.with_tx(tx).delete(key)`     | `b.Add(bucket.Deleted(ctx, key))`        |
| `bucket.withTx(tx).clear()`         | `bucket.with_tx(tx).clear()`         | `b.Add(bucket.Cleared(ctx))`             |

A batch only writes, so it doesn't read keys. Read them before the batch, or
after it. A bucket that lives in `kv.db`, a batch of another database and a
view (which only reads) are rejected with an invalid error (`ErrInvalid`,
`InvalidError`).

Outside a batch, the bucket works like any other bucket, with `get`, `set`,
expiry, versions and the rest.

## Things to know

- One SQL database holds one KV store. In Go, opening a second one `In` the
  same database fails with an in-use error (`ErrInUse`).
- The KV store opens after the database and closes before it. A backup of
  the store copies the buckets with the database.
- Counters, configs, limits and quotas of a store opened `In` a database live
  in that database's file too. In Go, only buckets of values take part in a
  batch.
- In Bun and Python, the server runs a batch as one transaction with its own
  disk sync. Combining SQL and KV avoids separate commits and makes the
  changes atomic.
- A bucket and its SQL batch must belong to the same store, even when
  another store has a database with the same name.
- In Go, preparing changes takes free memory without waiting. If the
  available budget cannot hold the changes and SQL arguments together,
  the batch fails with `ErrLimit`. Use a smaller batch or a larger budget.

## See also

- [Jobs and your data](../jobs/your-data.md): the same option for jobs.
- [Transactions](transactions.md): reads and writes of several keys at once.
- [kv/README.md](../../kv/README.md): the full contract.
