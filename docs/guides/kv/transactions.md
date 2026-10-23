# Transactions

A KV transaction reads keys, decides, and writes, all or nothing: selling the
last item of stock and recording the order together, so that two buyers can
never both take it. Writes that need no reads need no transaction: separate
calls already share commits.

```ts
const stock = store.bucket<number>('stock')
const orders = store.bucket<Order>('orders')

const placed = await store.tx(async tx => {
	const left = (await tx.with(stock).get(sku)) ?? 0
	if (left < 1) return false
	await tx.with(stock).set(sku, left - 1)
	await tx.with(orders).set(orderId, order)
	return true
})
```

```python
stock = store.bucket("stock", int)
orders = store.bucket("orders", Order)


async def sell(tx) -> bool:
    left = await tx.with_(stock).get(sku) or 0
    if left < 1:
        return False
    await tx.with_(stock).set(sku, left - 1)
    await tx.with_(orders).set(order_id, order)
    return True


placed = await store.tx(sell)
```

```go
stock, err := kv.OpenBucket[int64](store, "stock")
orders, err := kv.OpenBucket[Order](store, "orders")

err = store.Tx(ctx, func(tx *tinystore.Tx) error {
	left, _, err := stock.In(tx).Get(ctx, sku)
	if err != nil {
		return err
	}
	if left < 1 {
		return ErrSoldOut // rolls back
	}
	if err := stock.In(tx).Set(ctx, sku, left-1); err != nil {
		return err
	}
	return orders.In(tx).Set(ctx, orderID, order)
})
```

```rust
let stock = store.bucket::<i64>("stock").open()?;
let orders = store.bucket::<Order>("orders").open()?;

let placed = store.tx(|tx| -> Result<bool, ShopError> {
    let left = tx.with(&stock).get(&sku)?.unwrap_or(0);
    if left < 1 {
        return Ok(false);
    }
    tx.with(&stock).set(&sku, &(left - 1))?;
    tx.with(&orders).set(&order_id, &order)?;
    Ok(true)
})?;
```

## How it behaves

- `tx.with(handle)` takes a bucket or counters into the transaction (`with_`
  in Python, `handle.In(tx)` in Go). Every call inside goes through it.
- The function's return commits; a throw, an error or a panic rolls back
  (contract DUR-02).
- A call on the handle itself, `stock.get(sku)` instead of
  `tx.with(stock).get(sku)`, is `invalid` inside the function: it would read
  the file without the transaction's own writes.
- `store.tx` is a transaction of one file. Buckets live in `kv.db` and queues
  in `jobs.db`, so a handle of the other file is `invalid`. To commit keys and
  jobs together, open both from a database, `db.bucket` and `db.queue`, and
  use `db.tx`.
- In Rust, a write to a database from inside the function waits for that
  database's writer. If a transaction that holds that writer is waiting for
  `kv.db`'s, the write fails `unavailable` instead of waiting forever. Return
  the error so that the transaction rolls back, and run it again.
  `store.close()` from inside the function fails `invalid`.

## Across a connection

In Rust a transaction holds `kv.db`'s writer while it reads and decides. Over
the pipe or the network, a transaction never holds the writer while your
program awaits: its reads go at once, its writes are sent together when the
function returns, with a check that nothing it read has changed. If something
did, the function runs again, five times at most before `conflict`. So keep
the function free of side effects outside the store: it may run more than
once.

The values a transaction's `take`s hand back come in one answer, at most the
largest body the connection agreed, 2 MiB from every SDK. When they are more,
the writes have already committed, so the transaction fails with
`outcome unknown`, not `limit`: the keys are gone, and sending the
transaction again would take nothing and repeat its other writes. Take large
values one at a time, or read them before the transaction deletes them.

## See also

- [KV](README.md): the engine's overview.
- [Versions](versions.md): one key's read-modify-write without a
  transaction.
- [Two engines in one file](../../handbook/start/directory.md#two-engines-in-one-file):
  keys and jobs that commit with rows.
