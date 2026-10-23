# Jobs in your database

A queue opened from an application's SQL database keeps its jobs in that
database's file, so that a job commits with the rows it is about. A note saved
and the job that indexes it are both written, or neither: no outbox table and
no relay process.

```ts
const db = await store.database('app', { migrations: `${import.meta.dirname}/migrations` })
const indexing = db.queue<{ noteId: number }>('indexing')

await db.tx(async tx => {
	const note = await tx.exec`insert into notes (body) values (${body})`
	await tx.with(indexing).add({ noteId: note.lastInsertRowid }) // commits with the row, or not at all
})
indexing.work(async ({ noteId }) => index(noteId))
```

```python
db = await store.database("app", migrations=Path(__file__).parent / "migrations")
indexing = db.queue("indexing", Index)


async def save(tx: tinystore.SqlTx) -> None:
    note = await tx.exec("insert into notes (body) values (?)", body)
    await tx.with_(indexing).add(Index(note_id=note.last_insert_rowid))


await db.tx(save)
```

```go
db, err := sqldb.Open(ctx, store, "app", sqldb.Migrations(os.DirFS("migrations")))
indexing, err := jobs.OpenQueue[Index](db, "indexing")

err = db.Tx(ctx, func(tx *sqldb.Tx) error {
	note, err := tx.Exec(ctx, `insert into notes (body) values (?)`, body)
	if err != nil {
		return err
	}
	_, err = indexing.In(tx).Add(ctx, Index{NoteID: note.LastInsertRowid})
	return err
})
```

```rust
let db = store.database("app").migrations("migrations").open()?;
let indexing = db.queue::<Index>("indexing").open()?;

db.tx(|tx| -> tinystore::Result<()> {
    let note = tx.exec(sql!("insert into notes (body) values (?)", body))?;
    tx.with(&indexing).add(&Index { note_id: note.last_insert_rowid })?;
    Ok(())
})?;
```

## Add a job in a batch

When the job does not depend on what a read finds, add it in a batch instead
of a transaction. The queue's `adding` gives a job that `db.batch` writes with
its statements, in the same shared commit as other writes:

```ts
await db.batch([
	sql`update notes set body = ${body} where id = ${noteId}`,
	indexing.adding({ noteId }), // commits with the row, or not at all
])
```

```python
await db.batch([
    sql("update notes set body = ? where id = ?", body, note_id),
    indexing.adding(Index(note_id=note_id)),  # commits with the row, or not at all
])
```

```go
err = db.Batch(ctx,
	sqldb.SQL(`update notes set body = ? where id = ?`, body, noteID),
	indexing.Adding(Index{NoteID: noteID}), // commits with the row, or not at all
)
```

```rust
db.batch([
    sql!("update notes set body = ? where id = ?", body, note_id).into(),
    indexing.adding(&Index { note_id }), // commits with the row, or not at all
])?;
```

`adding` accepts the same options as `add`. If its id is taken, the job is
not added, and its entry in the batch's answer has `changes` set to 0.

A transaction holds the writer and pays for its own sync, so `db.tx` with
`tx.with(queue)` costs one sync per call and never groups with other writes.
Two separate commits, the row and then a job in `jobs.db`, do group, but a
crash between them keeps the row and loses the job. A batch is atomic and
still shares its commit. See [Writes and
transactions](../sql/writes.md#a-row-and-its-job-in-one-batch).

## How it behaves

- A job added in a transaction or a batch commits with its rows, or not at
  all, and a worker never sees it before the commit (contract JOB-02).
- A transaction takes a queue's `add`, `set`, `update`, `cancel` and `get`,
  and a batch takes its `adding`. `work`, `runDue`, `watch` and pages work
  outside them.
- The queue's handle and its calls are the same as a queue of the store. What
  differs is where its jobs live, and that they share the database's writer.
- The engine keeps its tables under names that start with `_tinystore_`,
  beside the application's, and its migrations apart from the application's.

## Why not two files

A queue of the store lives in `jobs.db`, with a writer of its own, and no
write is atomic across two files. Saving a row in `app.db` and then adding a
job to `jobs.db` leaves a moment where a crash keeps one without the other.
Most systems solve that with an outbox table and a process that copies rows
into the queue. A queue in the database's own file removes the moment instead.
The cost is that its jobs share the database's writer, which is the point.

## See also

- [Jobs](README.md): the engine's overview.
- [Save a message and its job together](../../handbook/chat/replies.md#save-a-message-and-its-job-together)
  in the Handbook.
