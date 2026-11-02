# An audit that commits with the change

An audit line has to exist exactly when the change it records does. Open the
stream from the database, `db.events('audit')`, and append inside the
database's transaction: the Records engine keeps the event with the rows, and
writes it to the stream once they commit. A change that rolls back leaves no
line, and a line is never lost to a crash between two writes.

```ts
const db = await store.database('app')
const audit = db.events('audit', { keep: '365d' })

await db.tx(async tx => {
	await tx.exec`update roles set role = ${'admin'} where user_id = ${42}`
	await tx.with(audit).append('role.changed', { userId: 42, role: 'admin' }) // kept with the update
})
const page = await audit.list({ newest: true, limit: 50 }) // the line is there once db.tx returns
```

```python
db = await store.database("app")
audit = db.events("audit", keep="365d")


async def promote(tx: tinystore.SqlTx) -> None:
    await tx.exec("update roles set role = ? where user_id = ?", "admin", 42)
    await tx.with_(audit).append("role.changed", {"user_id": 42, "role": "admin"})  # kept with the update


await db.tx(promote)
page = await audit.list(newest=True, limit=50)  # the line is there once db.tx returns
```

```go
db, err := sqldb.Open(ctx, store, "app")
audit, err := records.OpenEvents(db, "audit", records.Keep(365*24*time.Hour))

err = db.Tx(ctx, func(tx *sqldb.Tx) error {
	if _, err := tx.Exec(ctx, "update roles set role = ? where user_id = ?", "admin", 42); err != nil {
		return err
	}
	_, err := audit.In(tx).Append(ctx, "role.changed", records.Fields{"userId": 42, "role": "admin"}) // kept with the update
	return err
})
page, err := audit.List(ctx, records.Query{Newest: true, Limit: 50}) // the line is there once db.Tx returns
```

```rust
let db = store.database("app").open()?;
let audit = db.events("audit").keep(Duration::from_hours(365 * 24)).open()?;

db.tx(|tx| -> tinystore::Result<()> {
    tx.exec(sql!("update roles set role = ? where user_id = ?", "admin", 42))?;
    tx.with(&audit).append("role.changed", &RoleChanged { user_id: 42, role: "admin" })?; // kept with the update
    Ok(())
})?;
let page = audit.list().newest().limit(50).page()?; // the line is there once db.tx returns
```

The stream is the store's own stream, in `records.db`, like any other.
`store.events('audit')`, a read of every stream, a tail, `tinystore logs` and
the MCP tools all see its events. Opening it from the database only changes
where an append inside a transaction waits until the commit.

## What it guarantees

- If the transaction rolls back, its events never reach the stream.
- If the transaction commits, its events are in the stream when `db.tx`
  returns, and a read finds them at once.
- If the process crashes after the commit and before the events reach the
  stream, they wait in the database's file. The store writes them when the
  database's stream is opened again, or within a minute by its background
  maintenance. Each event is written once, even if a crash comes in the
  middle of that move (contract REC-13).

The events of one transaction reach the stream in one append, in the order
the transaction appended them.

## In a batch

A batch carries an event too, with `appending`. The event commits with the
batch's statements, and it reaches the stream before the batch returns.

```ts
await db.batch([
	sql`update roles set role = ${'admin'} where user_id = ${42}`,
	audit.appending('role.changed', { userId: 42, role: 'admin' }),
])
```

```python
await db.batch(
    [
        sql("update roles set role = ? where user_id = ?", "admin", 42),
        audit.appending("role.changed", {"user_id": 42, "role": "admin"}),
    ]
)
```

```go
err = db.Batch(ctx, sqldb.SQL("update roles set role = ? where user_id = ?", "admin", 42),
	audit.Appending("role.changed", records.Fields{"userId": 42, "role": "admin"}))
```

```rust
db.batch([
    sql!("update roles set role = ? where user_id = ?", "admin", 42).into(),
    audit.appending("role.changed", &RoleChanged { user_id: 42, role: "admin" }),
])?;
```

The batch says what each write did: an event's append changed 1, or 0 when
its stream already holds the event's id.

## Ids and times

Every event that waits for a commit has an id, so that writing it twice
writes it once. Give your own with the append's `id`, a change's or a
request's key. Without one, the store gives the event an id that no other
event gets, a UUID of version 7, and a read shows it.

```ts
await db.tx(async tx => {
	await tx.with(audit).append('role.changed', { userId: 42 }, { id: 'change-7f3a' }) // true
	await tx.with(audit).append('role.changed', { userId: 42 }, { id: 'change-7f3a' }) // false: the transaction holds the id
})
```

```python
async def promote(tx: tinystore.SqlTx) -> None:
    await tx.with_(audit).append("role.changed", {"user_id": 42}, id="change-7f3a")  # True
    await tx.with_(audit).append("role.changed", {"user_id": 42}, id="change-7f3a")  # False: the transaction holds the id


await db.tx(promote)
```

```go
err = db.Tx(ctx, func(tx *sqldb.Tx) error {
	_, err := audit.In(tx).Append(ctx, "role.changed", records.Fields{"userId": 42}, records.ID("change-7f3a")) // true
	return err
})
```

```rust
db.tx(|tx| {
    let first = tx.with(&audit).event("role.changed").id("change-7f3a").append(&RoleChanged { user_id: 42, role: "admin" })?; // true
    let again = tx.with(&audit).event("role.changed").id("change-7f3a").append(&RoleChanged { user_id: 42, role: "admin" })?; // false
    Ok::<_, tinystore::Error>((first, again))
})?;
```

Inside the transaction, `append` returns whether the event will be written:
`false` when the stream holds its id already, or the transaction appended an
event with that id before.

An event's time is taken at the append, inside the transaction, unless you
give `at`. The move keeps it. If the event waited longer than its stream's
`keep`, after a crash for example, the move drops it, as the stream's
retention would have, and the store logs it once.

## What it does not do

- **Atomic across files.** The event commits with the database's rows. A
  bucket, a queue or another database's stream, opened from the store or
  from another database, is another file. Open everything the transaction
  writes from the same database.
- **An append around the transaction.** `audit.append` outside `tx.with`
  goes to the stream at once, whether the rows commit or not. Called inside
  the transaction's function, it fails with an invalid error
  (`InvalidError`) in Rust, Bun, Node and Python. Go can't tell, so take
  the stream in with `In(tx)` there.
- **Reads inside the transaction.** A read of the stream goes through the
  stream itself, `audit.list`, and finds what has committed.

> [!WARNING]
> **If the move fails**
> When the stream refuses the events after the commit, for example on a full
> disk, the transaction still returns its result: the change committed. The
> events stay in the database's file, the store logs the failure once, and
> maintenance writes them within a minute after the disk has room.

## Where the events wait

Until its commit, an event waits in a table of the database's file,
`_tinystore_records_outbox`. Its schema is the Records engine's and is kept
apart from your migrations. The store removes each event from it once the
stream holds it. A backup carries the table with the rows, and a restore
writes what it holds at the next open of the stream.

A node can't keep a database's events yet: `db.events` fails with an
invalid error on a store that is a node.

## Limits and defaults

| What                                | Value                                                      |
|-------------------------------------|------------------------------------------------------------|
| an event's id when none is given    | a UUID of version 7, unique for good                       |
| an event's time when none is given  | the store's clock at the append                            |
| events left by a crash or a failure | written at the next open of the stream, or within a minute |
| an event its stream's `keep` passed | dropped by the move, logged once                           |
| an event                            | as any append: 256 KiB, and an id of 1 to 128 bytes        |

## See also

- [Events](events.md): appends, names, fields and ids.
- [Buckets and queues in a database](../sql/buckets-and-queues.md): what else
  commits with a database's rows.
- [Writes and transactions](../sql/writes.md): `db.tx` and `db.batch`.
