# Transactions

A transaction makes several KV writes succeed or fail together. Use a batch
when you know the writes in advance. Use `tx` when you need to read first and
decide what to write based on what you read.

## Write several keys together

```ts
await store.kv.batch(tx => {
	drafts.withTx(tx).of(userId).delete(noteId, { ifVersion: draft.version })
	published.withTx(tx).set(noteId, draft.value)
})
```

```python
async with store.kv.batch() as tx:
    drafts.of(user_id).with_tx(tx).delete(note_id, if_version=draft.version)
    published.with_tx(tx).set(note_id, draft.value)
```

```go
err = state.Tx(ctx, func(tx *kv.Tx) error {
	if err := drafts.WithTx(tx).Of(userID).Delete(ctx, noteID, kv.IfVersion(draft.Version)); err != nil {
		return err
	}
	return published.WithTx(tx).Set(ctx, noteID, draft.Value)
})
```

`withTx` gives you the bucket inside the transaction. If any write fails, for
example because a version doesn't match, none of the writes is saved.

A batch can use any buckets of the store. It can't include writes to other
engines, because each engine has its own file.

## Read, decide, then write

```ts
const userId = await store.kv.tx(async tx => {
	const userId = await codes.withTx(tx).take(digest(code)) // undefined: the code was used
	if (userId === undefined) {
		throw new InvalidCodeError()
	}
	sessions.withTx(tx).of(userId).set(digest(token), { device, since: Date.now() })
	return userId
})
```

```python
async def sign_in(tx: tinystore.Tx) -> int:
    user_id = await codes.with_tx(tx).take(digest(code))  # None: the code was used
    if user_id is None:
        raise InvalidCodeError
    sessions.of(user_id).with_tx(tx).set(digest(token), Session(device=device, since=int(time.time())))
    return user_id


user_id = await store.kv.tx(sign_in)
```

```go
err = state.Tx(ctx, func(tx *kv.Tx) error {
	userID, found, err := codes.WithTx(tx).Take(ctx, digest(code))
	if err != nil {
		return err
	}
	if !found {
		return errInvalidCode
	}
	return sessions.WithTx(tx).Of(userID).Set(ctx, digest(token), Session{Device: device, Since: time.Now()})
})
```

If two people click the same sign-in link at the same moment, only one of them
gets a session. The other finds the code gone.

When your function returns, the transaction commits. When it throws, or
returns an error in Go, nothing is saved.

In Go, the transaction runs your function on the store's writer. In Bun and
Python, nothing holds the writer while your code runs. A client that stopped
responding would block every other writer. The SDK works like this instead:

1. Each read goes to the server right away. The SDK remembers the version of
   every key that you read.
2. Each write waits until your function returns.
3. The SDK sends all writes in one batch. The batch first checks that every
   key you read still has the same version, and that every key you found
   absent is still absent.

If another program changed one of those keys in the meantime, the SDK runs
your function again with fresh reads, up to five times. After that, `tx` fails
with a conflict error (`ConflictError`).

> [!IMPORTANT]
> **Your function can run more than once**
> Only read and write keys inside `tx`. Send emails and call other services
> after `tx` returns.

Inside `tx`, a key that you wrote reads back what you wrote. A key that you
read twice gives the same answer both times. `tx` returns what your function
returns. It works with buckets of values. Counters, scans and other engines
can't be used in a `tx`.

## Read several keys from one moment

```ts
const [profile, settings] = await store.kv.view(tx => [
	profiles.withTx(tx).get(userId),
	prefs.withTx(tx).get(userId),
])
```

```python
async with store.kv.view() as tx:
    profile = profiles.with_tx(tx).get(user_id)
    settings = prefs.with_tx(tx).get(user_id)
print(await profile, await settings)
```

```go
err = state.View(ctx, func(tx *kv.Tx) error {
	profile, _, err = profiles.WithTx(tx).Get(ctx, userID)
	if err != nil {
		return err
	}
	settings, _, err = prefs.WithTx(tx).Get(ctx, userID)
	return err
})
```

A view reads all keys from the same snapshot, so no write can come between two
reads. A view can't write, and it can be held for at most five seconds.

In Bun, `view` and `batch` return what your function returns. If it returns an
array of calls, you get their results, as `Promise.all` would give them. In
Python, each call returns a future that you await after the block.

## Without a transaction

Most of the time you don't need one. Each call is already atomic:

- `take` reads and deletes a key in one step.
- `setIfAbsent` creates a key only if it doesn't exist.
- A write with `ifVersion` changes a key only if nobody else did.
- A [quota](quotas.md) checks and counts all of its windows in one write.

## Limits and defaults

|                                                       |                                |
|-------------------------------------------------------|--------------------------------|
| A view's snapshot                                     | 5 seconds                      |
| Runs of a Bun or Python `tx` whose keys keep changing | 5, then a conflict error       |
| `clear` of a branch inside a Go transaction           | up to 10,000 keys              |
| [Counters in memory](counters.md#count-in-memory)     | can't be used in a transaction |

## See also

- [Versions](versions.md): conditional writes without a transaction.
- [kv/README.md](../../kv/README.md): the full contract of transactions.
