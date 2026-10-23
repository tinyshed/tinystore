# Leases over a connection

A holder in the store's own process cannot die apart from the store: its key
is let go when its function returns, panics, or the process ends with the
store. A holder over a connection can: its process may freeze or crash while
the store goes on. So a lease given over a connection lasts its `ttl` from its
last renewal, and the SDK renews it while the function runs.

```ts
const summaries = store.lock('summaries', { ttl: '30s' })

await summaries.hold(chatId, async lease => {
	const text = await model.summarize(messages, { signal: lease.signal }) // aborted if the lease is lost
	await saveSummary(chatId, text, lease.fence)
})
```

```python
summaries = store.lock("summaries", ttl="30s")

async with summaries.hold(chat_id) as lease:  # a lost lease cancels the block at its next await
    text = await model.summarize(messages)
    await save_summary(chat_id, text, lease.fence)
```

```go
summaries, err := locks.OpenLock(store, "summaries", locks.TTL(30*time.Second))

err = summaries.Hold(ctx, chatID, func(ctx context.Context, lease *locks.Lease) error {
	text, err := model.Summarize(ctx, messages) // ctx ends when the lease is lost
	if err != nil {
		return err
	}
	return saveSummary(ctx, chatID, text, lease.Fence)
})
```

```rust
let summaries = store.lock("summaries").open()?;

summaries.hold(chat_id, |lease| {
    let text = model.summarize(&messages)?;
    if lease.lost() {
        return Err(Error::new(ErrorKind::Lost, "the summary was late")); // revoked at maxHold
    }
    save_summary(chat_id, &text, lease.fence())
})?;
```

## How a lease is kept

| Step                 | What happens                                                        |
|----------------------|---------------------------------------------------------------------|
| the key is given     | the server answers with the lease's id, its fence and its `maxHold` |
| every third of `ttl` | the SDK renews the lease, from the holder's own process             |
| a renewal fails once | not yet a loss: the SDK tries again until the deadline              |
| the deadline passes  | the holder counts the lease lost; the store gives the key on        |
| the function ends    | the SDK lets the key go, naming the lease                           |

- The holder's deadline is the time it sent the last renewal that succeeded,
  plus `ttl`, less a margin, so the holder gives up before the store does.
- A lease that ran out is never renewed (contract LOCK-05): holding the key
  again is a new lease with a new fence.
- A connection that drops leaves the lease to its deadline, and the SDK
  renews it by its id on the next connection.
- A program that froze stops renewing, since renewals come from its own
  process: its key goes to the next in line within `ttl`.

`ttl` is 10 seconds unless given, from 1 second to an hour. Short enough that
a crashed holder's key returns soon; long enough that a garbage collection
pause or a slow network does not lose a lease that is still working.

## When a lease is lost

A lease that ran out, or was revoked at `maxHold`, ends at that moment:

| Language   | The function hears it                                        |
|------------|--------------------------------------------------------------|
| TypeScript | `lease.signal` aborts: pass it to `fetch` and the model call |
| Python     | the block is cancelled at its next `await`                   |
| Go         | the function's `ctx` ends                                    |
| Rust       | `lease.lost()` turns true                                    |

Once the function ends, `hold` fails `lost`, even if the function
succeeded, so the caller learns that another holder may have run beside it.

The store cannot stop code that ignores the signal: a call the client already
made goes on. That is what [fences](fences.md) are for: the system it writes
to refuses the late write.

## Exactly one grant

A request to hold carries an id of its own. If the answer is lost on the way
back, the SDK asks again with the same id, on any connection, and the store
answers the lease it already gave instead of giving the key twice. That
answer also renews the lease for its `ttl`, because the SDK counts the
deadline from the answer. If the lease ran out before the SDK asked again,
the store ends it and treats the request as a new one, with a new fence. The
lease's own id, 128 random bits only its holder learns, is what renewing and
letting go name: a fence, which a holder hands to others, proves nothing.

A connection admitted to write may hold keys; one that only reads may not.

## A store that restarts

A store that opens again holds no lease. A client that held one through its
server learns at its next renewal that it lost it, and its next fence, from a
new epoch, is larger than every fence before the restart (contract LOCK-02).

## See also

- [Locks](README.md): the engine's overview.
- [Holding and waiting](holding.md): `hold`, `tryHold`, `wait`, `maxHold`.
- [Fences](fences.md).
