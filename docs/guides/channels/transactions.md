# Publishing from a transaction

A message published inside a transaction waits beside it and goes out after
its commit. A rollback sends nothing. So a subscriber that hears "order
created" can read the order: it is already committed.

```ts
await store.tx(async tx => {
	await tx.with(orders).set(id, order)
	await tx.with(events).under(id).publish({ kind: 'created' })
})
```

```python
async def create(tx: tinystore.Tx) -> None:
    await tx.with_(orders).set(id, order)
    await tx.with_(events).under(id).publish(Created(kind="created"))


await store.tx(create)
```

```go
err = store.Tx(ctx, func(tx *tinystore.Tx) error {
	if err := orders.In(tx).Set(ctx, id, order); err != nil {
		return err
	}
	return events.In(tx).Under(id).Publish(ctx, Created{Kind: "created"})
})
```

```rust
store.tx(|tx| -> tinystore::Result<()> {
    tx.with(&orders).set(&id, &order)?;
    tx.with(&events).under(&id).publish(&Created { kind: "created".into() })?;
    Ok(())
})?;
```

A channel keeps no file, so it joins any transaction: a store's, `store.tx`,
and a database's, `db.tx`:

```ts
await db.tx(async tx => {
	const reply = await tx.one<{ id: number }>`insert into messages (chat_id, text) values (${chatId}, ${text}) returning id`
	await tx.with(events).under(chatId).publish({ kind: 'message', id: reply!.id })
})
```

```python
async def reply(tx: tinystore.SqlTx) -> None:
    row = await tx.one("insert into messages (chat_id, text) values (?, ?) returning id", chat_id, text)
    await tx.with_(events).under(chat_id).publish(ChatEvent(kind="message", id=row["id"]))


await db.tx(reply)
```

```go
err = db.Tx(ctx, func(tx *sqldb.Tx) error {
	id, err := sqldb.Scalar[int64](ctx, tx, `insert into messages (chat_id, text) values (?, ?) returning id`, chatID, text)
	if err != nil {
		return err
	}
	return events.In(tx).Under(chatID).Publish(ctx, ChatEvent{Kind: "message", ID: id})
})
```

```rust
db.tx(|tx| -> tinystore::Result<()> {
    let id: i64 = tx.scalar(sql!("insert into messages (chat_id, text) values (?, ?) returning id", chat_id, text))?;
    tx.with(&events).under(chat_id).publish(&ChatEvent { kind: "message".into(), id })?;
    Ok(())
})?;
```

## What it promises

| The transaction                        | Its messages                                    |
|----------------------------------------|-------------------------------------------------|
| commits                                | delivered after the commit, in commit order     |
| rolls back, or throws                  | never delivered (contract CHAN-03)              |
| a call inside it fails, and it goes on | that call's messages dropped with its savepoint |

Messages of one file's transactions are handed over in the order of their
commits, at the writer, so two transactions committed one after the other
reach a subscriber in that order. Two files commit in no order between them.

A message published outside a transaction goes out at its call. A long
transaction can therefore release its message after one published later
outside it: order by commit, not by when `publish` was called.

## What it does not promise

A message released after a commit lives in memory. A process that dies
between the commit and the release loses it, and a subscriber that was not
listening never gets it (NOT-02). For an event that must go out with its rows
whatever happens, an email, a webhook, put a job in the same database, which
commits with them:

| Need                                            | Use                                                     |
|-------------------------------------------------|---------------------------------------------------------|
| open tabs refresh now; a lost one reloads       | a channel                                               |
| something must happen, once, even after a crash | [a queue in the database](../sql/buckets-and-queues.md) |
| a history to read again later                   | [records](../records/README.md)                         |

## Through a server

A database's transaction over a connection carries its messages to the
server, which releases them at its commit. A store's transaction commits in
one request, and the SDK sends its messages once that commit answered. Either
way a subscriber never hears a message before its commit.

## See also

- [Channels](README.md): the engine's overview.
- [Subscriptions and missed messages](subscriptions.md).
- [Transactions](../kv/transactions.md) of the store's buckets and queues.
