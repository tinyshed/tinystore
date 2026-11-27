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
  `wait` at most. Urgent holds stand before background ones, as the next
  section shows.
- `limit` is 1 to 1,000. A semaphore of `limit: 1` is a lock without fences.

## Urgent and background

Every request to an outside service goes through one semaphore. Background
jobs should not fill its line while a person waits for an answer. Ask for the
jobs' places in the background:

```ts
const cabinet = store.semaphore('cabinet', { limit: 8, background: 4 })

await cabinet.hold('requests', permit => syncGrades(permit.signal), {
	priority: 'background',
	wait: '60s',
	timeout: '15s',
})
await cabinet.hold('requests', permit => fetchProfile(user.id, permit.signal)) // urgent: passes the background line
```

```python
cabinet = store.semaphore("cabinet", limit=8, background=4)

async with cabinet.hold("requests", priority="background", wait="60s", timeout="15s"):
    await sync_grades()
async with cabinet.hold("requests"):  # urgent: passes the background line
    await fetch_profile(user.id)
```

```go
cabinet, err := locks.OpenSemaphore(store, "cabinet", locks.Limit(8), locks.Background(4))

err = cabinet.Hold(ctx, "requests", syncGrades,
	locks.InBackground(), locks.Wait(time.Minute), locks.Timeout(15*time.Second))
err = cabinet.Hold(ctx, "requests", fetchProfile) // urgent: passes the background line
```

```rust
let cabinet = store.semaphore("cabinet").limit(8).background(4).open()?;

cabinet
    .key("requests")
    .background()
    .wait(Duration::from_secs(60))
    .timeout(Duration::from_secs(15))
    .hold(|_| sync_grades())?;
cabinet.hold("requests", |_| fetch_profile(&user.id))?; // urgent: passes the background line
```

- A hold is urgent unless you ask for the background. Every hold you wrote
  before this option is urgent.
- When a place frees, the first urgent hold in line gets it. Background holds
  get a place only when no urgent hold waits.
- A background hold in line never blocks an urgent one. An urgent hold that
  finds a free place takes it at once, even if background holds wait.
- `background` is how many of a key's places background holds can take at
  once, 1 to `limit`. With 8 places and `background: 4`, urgent holds always
  find at least 4 places that background work cannot take. Without it,
  background holds can take every place.
- Within each priority, holds are served in the order they came (contracts
  LOCK-01, LOCK-09 and LOCK-10).
- A background hold is not promised a place. While urgent holds keep coming
  and take the room, it waits, and its `wait` ends that (NOT-13).
- Inside one program, a name has one `background`, as it has one `limit`. A
  second open with another value fails `invalid` and names both.

`timeout` is how long one hold may keep its place. It counts from the grant,
not from the call, so the time spent in line does not shorten the work's
time. Past it, the place goes to the next in line and the hold fails as at
`maxHold`: with `LostError` in Bun and Python, `ErrLost` in Go and
`ErrorKind::Lost` in Rust, even if the work finished. The permit's signal,
the context in Go, aborts at that moment (contract LOCK-11). When the
semaphore also has a `maxHold`, the shorter of the two counts.

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

## When what it guards is down

A semaphore sees only whether your function threw, not why. When the service
behind it is down, every waiter still sits out its own timeout. To stop
calling it for a while, keep the decision in a key with a `ttl`, which every
program of the store shares:

```ts
const strikes = store.counters('strikes', { ttl: '1m' })
const broken = store.bucket<true>('broken')

if (await broken.has('moodle')) throw new Unavailable()
try {
	return await moodle.hold('requests', permit => fetchMoodle(permit.signal))
} catch (err) {
	if (isTimeout(err) && (await strikes.add('moodle')) >= 2) await broken.set('moodle', true, { ttl: '30s' })
	throw err
}
```

```python
strikes = store.counters("strikes", ttl="1m")
broken = store.bucket("broken", bool)

if await broken.has("moodle"):
    raise Unavailable()
try:
    async with moodle.hold("requests"):
        return await fetch_moodle()
except TimeoutError:
    if await strikes.add("moodle") >= 2:
        await broken.set("moodle", True, ttl="30s")
    raise
```

```go
strikes, err := kv.OpenCounters(store, "strikes", kv.TTL(time.Minute))
broken, err := kv.OpenBucket[bool](store, "broken")

if down, _ := broken.Has(ctx, "moodle"); down {
	return errUnavailable
}
err = moodle.Hold(ctx, "requests", fetchMoodle)
if errors.Is(err, context.DeadlineExceeded) {
	if n, _ := strikes.Add(ctx, "moodle", 1); n >= 2 {
		_ = broken.Set(ctx, "moodle", true, kv.TTL(30*time.Second))
	}
}
return err
```

```rust
let strikes = store.counters("strikes").ttl(Duration::from_secs(60)).open()?;
let broken = store.bucket::<bool>("broken").open()?;

if broken.has("moodle")? {
    return Err(unavailable());
}
let fetched = moodle.hold("requests", |_| fetch_moodle());
if fetched.as_ref().is_err_and(is_timeout) && strikes.add("moodle", 1)? >= 2 {
    broken.key("moodle").ttl(Duration::from_secs(30)).set(&true)?;
}
fetched
```

Two timeouts within a minute stop every program's calls for 30 seconds; the
key expires by itself and the next call tries again.

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
