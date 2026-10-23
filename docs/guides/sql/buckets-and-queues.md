# Buckets and queues in a database

A bucket or a queue opened from an application's database lives in that
database's file, beside its tables, and commits with its rows. An order and
the cart it empties, a note and the job that indexes it: both written, or
neither.

```ts
const db = await store.database('shop', { migrations: `${import.meta.dirname}/migrations` })
const carts = db.bucket<string[]>('carts', { idle: '7d' })
const receipts = db.queue<{ orderId: number }>('receipts')

await db.tx(async tx => {
	const order = await tx.one<{ id: number }>`insert into orders (user_id, total, status, created_at)
		values (${userId}, ${total}, 'paid', ${Date.now()}) returning id`
	await tx.with(carts).delete(String(userId)) // the cart is emptied with the order, or not at all
	await tx.with(receipts).add({ orderId: order!.id }) // the receipt is sent only for an order that exists
})
```

```python
db = await store.database("shop", migrations=Path(__file__).parent / "migrations")
carts = db.bucket("carts", list[str], idle="7d")
receipts = db.queue("receipts", Receipt)


async def checkout(tx: tinystore.SqlTx) -> None:
    order = await tx.one(
        "insert into orders (user_id, total, status, created_at) values (?, ?, 'paid', ?) returning id",
        user_id, total, time.time_ns() // 1_000_000,
    )
    await tx.with_(carts).delete(str(user_id))
    await tx.with_(receipts).add(Receipt(order_id=order["id"]))


await db.tx(checkout)
```

```go
db, err := sqldb.Open(ctx, store, "shop", sqldb.Migrations(os.DirFS("migrations")))
carts, err := kv.OpenBucket[[]string](db, "carts", kv.Idle(7*24*time.Hour))
receipts, err := jobs.OpenQueue[Receipt](db, "receipts")

err = db.Tx(ctx, func(tx *sqldb.Tx) error {
	orderID, err := sqldb.Scalar[int64](ctx, tx, `insert into orders (user_id, total, status, created_at)
		values (?, ?, 'paid', ?) returning id`, userID, total, time.Now().UnixMilli())
	if err != nil {
		return err
	}
	if _, err := carts.In(tx).Delete(ctx, strconv.FormatInt(userID, 10)); err != nil {
		return err
	}
	_, err = receipts.In(tx).Add(ctx, Receipt{OrderID: orderID})
	return err
})
```

```rust
let db = store.database("shop").migrations("migrations").open()?;
let carts = db.bucket::<Vec<String>>("carts").idle(Duration::from_hours(24 * 7)).open()?;
let receipts = db.queue::<Receipt>("receipts").open()?;

db.tx(|tx| -> tinystore::Result<()> {
    let order_id: i64 = tx.scalar(sql!(
        "insert into orders (user_id, total, status, created_at) values (?, ?, 'paid', ?) returning id",
        user_id,
        total,
        unix_millis(SystemTime::now())
    ))?;
    tx.with(&carts).delete(&user_id.to_string())?;
    tx.with(&receipts).add(&Receipt { order_id })?;
    Ok(())
})?;
```

## What joins a transaction

| Handle                   | Inside `tx.with(…)`                                     | Outside, as usual      |
|--------------------------|---------------------------------------------------------|------------------------|
| a bucket of the database | `get`, `set`, `delete` and the rest of a bucket's calls | every call             |
| a queue of the database  | `add`, `set`, `update`, `cancel`, `get`                 | `work`, `watch`, pages |
| a channel                | `publish`, handed over at the commit                    | subscriptions          |

What the transaction writes through them commits with the rows, or not at
all, and what it reads sees what the transaction wrote. A worker never sees a
job before its commit (contract JOB-02), and a subscriber never receives a
message of a transaction that rolled back.

A job that does not depend on a read can go in a batch instead, with the
queue's `adding`. A batch shares its commit with other writes, and a
transaction does not. See [A row and its job in one
batch](writes.md#a-row-and-its-job-in-one-batch).

A bucket or a queue of the store, `store.bucket(…)`, lives in `kv.db` or
`jobs.db`, another file with another writer, so it cannot join a database's
transaction. Open it from the database instead.

## The same handle, another file

Outside a transaction, a database's bucket and queue are the handles of the
[KV](../kv/README.md) and [Jobs](../jobs/README.md) guides, with the same
calls: expiry, versions, workers, retries. What differs is where they live:

- their tables sit in `sql/<name>.db` under names that start with
  `_tinystore_`, and their schema is the engine's, kept apart from your
  migrations;
- they share the database's writer: a busy queue's claims wait behind the
  application's writes, and the other way round, which is the point and its
  cost;
- a backup, a copy on another node and a restore carry them with the rows.

## A table's index

A search index can live in the database too, following a table: every change
of a row reaches it in the same commit. See [A table's
index](../search/table-index.md).

## Why not two files

Every engine has a file of its own so that no engine waits on another's
writer. The price is that no write is atomic across two files: saving an
order in `shop.db` and then adding a job to `jobs.db` leaves a moment when a
crash keeps one without the other. The usual cure is an outbox table and a
process that copies its rows into the queue. Opening the queue from the
database removes the moment instead, by the application's choice and never by
default.

## See also

- [SQL](README.md): the engine's overview.
- [Jobs in your database](../jobs/your-database.md): the same from the
  queue's side.
- [Two engines in one file](../../handbook/start/directory.md#two-engines-in-one-file)
  in the Handbook.
