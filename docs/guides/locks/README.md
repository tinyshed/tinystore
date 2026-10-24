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

## Async Rust

With the `async` feature, a Rust program opens the same locks and semaphores
from `tinystore::aio::Store`, and a hold is awaited. This section shows only
Rust, because in Bun, Node and Python every call is already awaited, and in
Go a call blocks its goroutine, which costs little.

```rust
use tinystore::aio::Store;

let store = Store::open("data", Options::default())?;
let summaries = store.lock("summaries").max_hold(Duration::from_secs(120)).open()?;

// blocking: summaries.hold(chat_id, |lease| write_summary(chat_id, lease.fence()))?
summaries.hold(chat_id, |lease| async move { write_summary(chat_id, lease.fence()).await }).await?;
let tried = summaries.try_hold(chat_id, |lease| async move { write_summary(chat_id, lease.fence()).await }).await?;
summaries
    .key(chat_id)
    .wait(Duration::from_secs(30))
    .hold(|lease| async move { write_summary(chat_id, lease.fence()).await })
    .await?;

let uploads = store.semaphore("uploads").limit(3).open()?;
uploads.hold(user_id, |_permit| async move { upload(user_id).await }).await?;
```

- The function you pass gets the lease by value and returns a future, which
  runs while the key is held. The key goes to the next holder when that
  future ends, fails or is dropped.
- While the key is held by someone else, the hold waits in the key's line,
  first come first served, and blocks no thread. If the key is still held
  after `wait`, ten seconds unless you say, the hold fails with an in use
  error (`ErrorKind::InUse`).
- If you drop the hold's future while it waits, it leaves the line. So
  `tokio::time::timeout(d, summaries.hold(…))` works beside `wait`.
- If the lease ends while your future runs, because of `max_hold` or because
  the store closed, `lease.lost()` returns true and the hold fails with a
  lost error (`ErrorKind::Lost`), as in blocking Rust.
- If you keep the lease after the hold, by returning it or by moving it into
  a task, `lease.lost()` returns true once the hold ends. Another holder may
  have the key by then.
- A hold of a key inside its own hold is refused with an invalid error
  (`ErrorKind::Invalid`), and so is a hold inside a transaction's closure.
  A hold in a task that you spawn waits like any other holder.

[Rust with async](../../handbook/start/rust-async.md) explains where the work
runs and what dropping a future does.

## Reference

[Bun and Node](../../reference/bun/locks.md) ·
[Python](../../reference/python/locks.md) ·
[Go](../../reference/go/locks.md) ·
[Rust](../../reference/rust/locks.md)
