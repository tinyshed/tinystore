# Locks

The Locks engine gives one holder of a key at a time, across every process
and connection of a store: one summary per chat, one charge per invoice.
Waiters are served in order, a lease over a connection is renewed while its
holder works, and every lease carries a fence, a number that only grows, so
that a holder that stalled past its lease can be refused.

```ts
const summaries = store.lock('summaries', { ttl: '30s' })

await summaries.hold(chatId, async lease => {
	await writeSummary(chatId, lease.fence)
})
const tried = await summaries.tryHold(chatId, lease => writeSummary(chatId, lease.fence))
if (!tried.acquired) return // somebody holds it
```

```python
summaries = store.lock("summaries", ttl="30s")

async with summaries.hold(chat_id) as lease:
    await write_summary(chat_id, lease.fence)
async with summaries.try_hold(chat_id) as lease:
    if lease is None:
        return  # somebody holds it
    await write_summary(chat_id, lease.fence)
```

```go
summaries, err := locks.OpenLock(store, "summaries", locks.TTL(30*time.Second))

err = summaries.Hold(ctx, chatID, func(ctx context.Context, lease *locks.Lease) error {
	return writeSummary(ctx, chatID, lease.Fence)
})
acquired, err := summaries.TryHold(ctx, chatID, func(ctx context.Context, lease *locks.Lease) error {
	return writeSummary(ctx, chatID, lease.Fence)
})
```

```rust
let summaries = store.lock("summaries").open()?;

summaries.hold(chat_id, |lease| write_summary(chat_id, lease.fence()))?;
let tried = summaries.try_hold(chat_id, |lease| write_summary(chat_id, lease.fence()))?; // None if held
```

## What it guarantees

- A key has one holder at a time, given to waiters in the order they came
  (contract LOCK-01).
- Every lease's fence is larger than every earlier lease's, across restarts
  (contract LOCK-02).
- A lease over a connection lasts its `ttl` from its last renewal; one that
  ran out is never renewed (contract LOCK-05).
- A panic, a cancel or the store's close lets the key go and answers every
  waiter (contract LOCK-06).
- Not promised: locks live in the memory of the process that holds the
  store, and do not coordinate two stores (NOT-03).

## In these guides

| Page                                  | Covers                                         |
|---------------------------------------|------------------------------------------------|
| [Holding and waiting](holding.md)     | `hold`, `tryHold`, `wait`, `maxHold`           |
| [Leases over a connection](leases.md) | `ttl`, renewal, a lost lease                   |
| [Fences](fences.md)                   | refusing a late write in SQL or another system |
| [Semaphores](semaphores.md)           | up to N holders of a key                       |

New to Locks? [Live updates](../../handbook/chat/live-updates.md#one-summary-per-chat-at-a-time)
in the Handbook writes one summary per chat at a time, with a fence in SQL.

## Reference

[Bun and Node](../../reference/bun/locks.md) ·
[Python](../../reference/python/locks.md) ·
[Go](../../reference/go/locks.md) ·
[Rust](../../reference/rust/locks.md)
