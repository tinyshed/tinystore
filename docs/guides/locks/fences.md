# Fences

A lock makes two holders at once rare; a fence makes a late one harmless.
Every lease carries a fence, a number larger than every earlier lease's. A
system that keeps the largest fence it has seen for a key refuses a write
from a holder whose lease ran out while it was paused.

```scene
code ts
  await summaries.hold(chatId, async lease => save(chatId, text, lease.fence))
end
actor a     "Worker A"  at 10,25 icon=cpu
actor b     "Worker B"  at 10,75 icon=cpu
actor lock  "Lock"      at 50,50 icon=lock
actor db    "chat.db"   at 90,50 icon=database
link a lock
link b lock
link a db
link b db
set db "fence 0"

step "Worker A holds the chat's key with fence 7, then pauses: a long garbage collection." line=1
  send lock -> a "fence 7"
  set a "paused"

step "A's lease runs out. Worker B holds the key with fence 8 and saves its summary." line=1
  send lock -> b "fence 8"
  send b -> db "save, fence 8" color=ok
  set db "fence 8"

step "A wakes and saves too. The database keeps the largest fence, 8, so it refuses A's write." line=1
  send a -> db "save, fence 7"
  strike a "paused"
  send db -> a "refused: 7 < 8" color=bad
```

## Checking a fence in SQL

A fence is a 128-bit number, too large for an SQLite integer. Written as 40
zero-padded digits, it compares as text in the same order:

```sql title="migrations/0004_summary_fence.sql"
alter table chats add column summary_fence text not null default '';
```

```ts
await summaries.hold(chatId, async lease => {
	const summary = await model.summarize(await history(chatId))
	const fence = lease.fence.toString().padStart(40, '0')
	const done = await db.exec`update chats set summary = ${summary}, summary_fence = ${fence}
		where id = ${chatId} and summary_fence < ${fence}`
	if (done.changes === 0) return // a later holder already wrote
})
```

```python
async with summaries.hold(chat_id) as lease:
    summary = await model.summarize(await history(chat_id))
    fence = f"{lease.fence:040d}"
    await db.exec(
        "update chats set summary = ?, summary_fence = ? where id = ? and summary_fence < ?",
        summary, fence, chat_id, fence,
    )
```

```go
err = summaries.Hold(ctx, chatID, func(ctx context.Context, lease *locks.Lease) error {
	summary, err := model.Summarize(ctx, history(ctx, chatID))
	if err != nil {
		return err
	}
	fence := fmt.Sprintf("%040s", lease.Fence)
	_, err = db.Exec(ctx, `update chats set summary = ?, summary_fence = ? where id = ? and summary_fence < ?`,
		summary, fence, chatID, fence)
	return err
})
```

```rust
summaries.hold(chat_id, |lease| -> Result<(), AppError> {
    let summary = model.summarize(&history(chat_id)?)?;
    let fence = format!("{:040}", lease.fence().get());
    db.exec(sql!(
        "update chats set summary = ?, summary_fence = ? where id = ? and summary_fence < ?",
        summary,
        &fence,
        chat_id,
        &fence
    ))?;
    Ok(())
})?;
```

The check and the write are one statement, so nothing can come between them.

## Fences in each language

| Language   | A fence is                                                                |
|------------|---------------------------------------------------------------------------|
| TypeScript | a `bigint`, every digit kept                                              |
| Python     | an `int`                                                                  |
| Go         | a `locks.Fence` of two `uint64`s, compared by `Less`, printed by `String` |
| Rust       | an ordered `Fence`, its value through `get()`                             |
| the wire   | a decimal string                                                          |

## What a fence promises

A fence is two numbers compared in order: the store's epoch in the high 64
bits, a count in the low. The count grows with every lease the store gives
while it is open; the epoch is written to kv.db each time the store opens,
as the larger of the time in milliseconds and the epoch before it plus one.

| Between                                       | A later lease's fence is larger                                                    |
|-----------------------------------------------|------------------------------------------------------------------------------------|
| two leases of one store while it is open      | always                                                                             |
| a store and itself after a crash or a restart | always (contract LOCK-02)                                                          |
| a store and one restored from a backup        | only while the restoring machine's clock is past the epochs given since the backup |

A semaphore's leases have no fence: three valid holders at once would refuse
each other.

## Fences outside the store

A fence protects only where something compares it. Send it to any system that
can keep the largest one per key, a header your own service checks, a
conditional write in object storage. A payment provider that compares nothing
needs an idempotency key all the same: a lock makes two attempts at once
rarer, and only the provider's own deduplication makes a charge happen once.

## See also

- [Locks](README.md): the engine's overview.
- [Leases over a connection](leases.md): when a lease runs out.
- [One summary per chat at a time](../../handbook/chat/live-updates.md#one-summary-per-chat-at-a-time)
  in the Handbook.
