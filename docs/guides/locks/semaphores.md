# Semaphores

A semaphore is a lock whose key up to `limit` holders may hold at once: three
uploads per user, two calls to a model per workspace, eight exports on the
whole server. The calls are a lock's: `hold`, `tryHold`, `wait`, `maxHold`,
a lease renewed over a connection.

```ts
const uploads = store.semaphore('uploads', { limit: 3, ttl: '30s' })

await uploads.hold(user.id, () => upload(file)) // the fourth upload of a user waits for a place
const tried = await uploads.tryHold(user.id, () => upload(file))
if (!tried.acquired) return tooMany()
```

```python
uploads = store.semaphore("uploads", limit=3, ttl="30s")

async with uploads.hold(user.id):
    await upload(file)
async with uploads.try_hold(user.id) as permit:
    if permit is None:
        return too_many()
    await upload(file)
```

```go
uploads, err := locks.OpenSemaphore(store, "uploads", locks.Limit(3), locks.TTL(30*time.Second))

err = uploads.Hold(ctx, user.ID, func(ctx context.Context, _ *locks.Lease) error {
	return upload(ctx, file)
})
acquired, err := uploads.TryHold(ctx, user.ID, func(ctx context.Context, _ *locks.Lease) error {
	return upload(ctx, file)
})
```

```rust
let uploads = store.semaphore("uploads").limit(3).open()?;

uploads.hold(&user.id, |_| upload(&file))?;
let tried = uploads.try_hold(&user.id, |_| upload(&file))?; // None when three hold it
```

- Each key has its own places: user 42's three uploads never take user 43's.
  For one bound over everything, use one key: `uploads.hold('all', …)`.
- Waiters stand in line for the next free place, first come first served,
  `wait` at most.
- `limit` is 1 to 1,000. A semaphore of `limit: 1` is a lock without fences.

## No fences

A semaphore's lease has no fence. Three holders are valid at once, and a
system that refused all but the largest fence would refuse two of them. A
holder that stalled past its lease is told `lost` as a lock's is, through its
signal, its context or `lost()`.

## Semaphore, rate limit or concurrency?

| You want                                           | Use                                               |
|----------------------------------------------------|---------------------------------------------------|
| at most N running at once, anywhere in the program | a semaphore                                       |
| at most N starting per second or minute            | a [rate limit](../kv/rate-limits.md)              |
| at most N of a queue's jobs running, by group      | the queue's [concurrency](../jobs/concurrency.md) |
| uses counted against a plan, refunded on failure   | a [quota](../kv/quotas.md)                        |

A semaphore counts what runs now and forgets it when it ends; a rate limit
counts what started recently, even if it finished. A queue's concurrency is
taken in the same transaction as the job, so it survives restarts; a
semaphore lives in memory.

## Two opens with different limits

Inside one program a name has one limit. A second open of the name with
another limit fails `invalid` at the open and names both limits. An open with
the same limit is fine.

```ts
const gpu = store.semaphore('gpu', { limit: 1 })
store.semaphore('gpu', { limit: 3 }) // InvalidError: semaphore gpu: open in this program with a limit of 1, not 3
```

```python
gpu = store.semaphore("gpu", limit=1)
store.semaphore("gpu", limit=3)  # InvalidError: semaphore gpu: open in this program with a limit of 1, not 3
```

```go
gpu, err := locks.OpenSemaphore(store, "gpu", locks.Limit(1))
_, err = locks.OpenSemaphore(store, "gpu", locks.Limit(3)) // tinystore.ErrInvalid: ... with a limit of 1, not 3
```

```rust
let gpu = store.semaphore("gpu").limit(1).open()?;
let wider = store.semaphore("gpu").limit(3).open(); // Err: invalid, open in this program with a limit of 1, not 3
```

A program here is one store you opened: two stores opened on one directory
are two programs, as two processes are (contract LOCK-07).

Between programs, the limit is given where each one opens the semaphore, and
a program may change it between deploys. A key admits by the limit of the
open that waits: a smaller limit lets no holder go, it only lets fewer in
until the holders above it finish.

## See also

- [Locks](README.md): the engine's overview.
- [Holding and waiting](holding.md): `hold`, `tryHold`, `wait`, `maxHold`.
