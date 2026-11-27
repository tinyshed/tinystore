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
  for at most `wait`. Urgent holds go before background ones, as the next
  section shows.
- `limit` is 1 to 1,000. A semaphore of `limit: 1` is a lock without fences.

## Urgent and background

Say every request to an outside service goes through one semaphore.
Background jobs should not fill its line while a person waits for an answer.
So mark the jobs' holds as background:

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

- A hold is urgent unless you mark it as background. So every hold you
  wrote before this option existed is urgent.
- When a place frees up, the first urgent hold in line gets it. Background
  holds get a place only when no urgent hold is waiting.
- A background hold in line never blocks an urgent one. If an urgent hold
  finds a free place, it takes it at once, even if background holds are
  waiting.
- `background` sets how many of a key's places background holds can take at
  once, from 1 to `limit`. With 8 places and `background: 4`, urgent holds
  always have at least 4 places that background work cannot take. Without
  this option, background holds can take every place.
- Within each priority, holds are served in the order they arrived
  (contracts LOCK-01, LOCK-09 and LOCK-10).
- A background hold is not guaranteed a place. While new urgent holds keep
  taking the free places, it waits until its `wait` runs out (NOT-13).
- Inside one program, a name has one `background` value, just as it has one
  `limit`. A second open with a different value fails with `invalid`, and
  the error shows both values.

`timeout` sets how long one hold may keep its place. It counts from the
moment the hold gets its place, not from the call, so time spent in line
does not shorten the time for the work. When the timeout passes, the place
goes to the next in line, and the hold fails the same way as at `maxHold`.
The error is `LostError` in Bun and Python, `ErrLost` in Go and
`ErrorKind::Lost` in Rust, even if the work finished. At that moment the
permit's signal aborts, or the context in Go (contract LOCK-11). If the
semaphore also has a `maxHold`, the shorter of the two applies.

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

## A pause when what it guards is down

When the service behind a semaphore is down, every caller waits for its own
timeout. Give the semaphore a breaker instead. After a set number of failed
holds in a row, the key is down for a pause, and every caller learns it at
once:

```ts
const cabinet = store.semaphore('cabinet', {
	limit: 8,
	background: 4,
	breaker: { failures: 2, pause: '30s' }, // two failed holds in a row: nobody is let in for 30 s
})

try {
	return await cabinet.hold('requests', permit => fetchGrades(user.id, permit.signal), { timeout: '15s' })
} catch (err) {
	if (err instanceof UnavailableError) return showCached(user.id) // the cabinet is down
	throw err
}
```

```python
cabinet = store.semaphore("cabinet", limit=8, background=4, breaker=Breaker(failures=2, pause="30s"))

try:
    async with cabinet.hold("requests", timeout="15s"):
        return await fetch_grades(user.id)
except UnavailableError:
    return show_cached(user.id)  # the cabinet is down
```

```go
cabinet, err := locks.OpenSemaphore(store, "cabinet",
	locks.Limit(8), locks.Background(4), locks.Breaker(2, 30*time.Second))

err = cabinet.Hold(ctx, "requests", fetchGrades, locks.Timeout(15*time.Second))
if errors.Is(err, tinystore.ErrUnavailable) {
	return showCached(user.ID) // the cabinet is down
}
```

```rust
let cabinet = store.semaphore("cabinet").limit(8).background(4).breaker(2, Duration::from_secs(30)).open()?;

match cabinet.key("requests").timeout(Duration::from_secs(15)).hold(|_| fetch_grades(&user.id)) {
    Err(error) if error.kind() == ErrorKind::Unavailable => show_cached(&user.id), // the cabinet is down
    fetched => fetched,
}
```

- A hold fails when its function throws or returns an error, or when it runs
  past its `timeout` or `maxHold`. A hold that succeeds resets the count. A
  quiet period with no failures resets it too: if a failure comes `pause` or
  more after the previous one, it counts as the first.
- After `failures` failed holds of a key in a row, the key is down for
  `pause`. Every hold of that key, waiting or new, fails at once with an
  unavailable error: `UnavailableError` in Bun and Python,
  `ErrUnavailable` in Go and `ErrorKind::Unavailable` in Rust. It is never an
  in-use error, and its message says since when the key has been down
  (contract LOCK-12).
- When the pause ends, one hold goes through and the others still fail. If it
  succeeds, the key is up again. If it fails, the key is down for another
  pause. If its program goes away before the hold ends, the next hold goes
  through instead (contract LOCK-13).
- If a hold's program goes away and its lease runs out, the hold does not
  count as a failure, because the service did not fail. In Rust with async,
  a hold whose future you drop does not count either.
- Each key has its own breaker, just as it has its own places.

Some errors are yours, not the service's, for example a student who does
not exist. Call `healthy` to say that the service is up. Then the hold does
not count as a failure, even if the function throws afterwards:

```ts
await cabinet.hold('requests', async permit => {
	const response = await fetch(url, { signal: permit.signal })
	if (response.status === 404) {
		permit.healthy() // the cabinet answered: it is up
		throw new NotFound(user.id)
	}
	return response.json()
})
```

```python
async with cabinet.hold("requests") as permit:
    response = await client.get(url)
    if response.status_code == 404:
        permit.healthy()  # the cabinet answered: it is up
        raise NotFound(user.id)
```

```go
err = cabinet.Hold(ctx, "requests", func(ctx context.Context, permit *locks.Lease) error {
	response, err := get(ctx, url)
	if err == nil && response.StatusCode == http.StatusNotFound {
		permit.Healthy() // the cabinet answered: it is up
		return errNotFound
	}
	return err
})
```

```rust
cabinet.hold("requests", |permit| {
    let response = get(&url)?;
    if response.status() == 404 {
        permit.healthy(); // the cabinet answered: it is up
        return Err(not_found(&user.id));
    }
    Ok(response)
})?;
```

### Whether it is up

`state` returns the key's current status, for example for a status line:

```ts
const { up, downSince, lastFailure, waiting, holding } = await cabinet.state('requests')
if (!up) banner(`The cabinet is down since ${downSince?.toLocaleTimeString()}`)
```

```python
state = await cabinet.state("requests")
if not state.up:
    banner(f"The cabinet is down since {state.down_since:%H:%M}")
```

```go
state, err := cabinet.State(ctx, "requests")
if !state.Up {
	banner("The cabinet is down since " + state.DownSince.Format(time.Kitchen))
}
```

```rust
let state = cabinet.state("requests")?;
if !state.up {
    banner(&format!("The cabinet is down since {:?}", state.down_since));
}
```

`up` is false from the failure that put the key down until a hold succeeds
after the pause. `lastFailure / last_failure / LastFailure / last_failure`
stays set until a hold succeeds after it. While the key is up, it is also
cleared once a `pause` passes after it. `waiting` counts the holds in line,
and `holding` counts the holds that have a place. Both work without a
breaker too, and then `up` is always true (contract LOCK-14).

### What a breaker does not do

- The breaker lives in the memory of the process that holds the store, next
  to the lines. Every program of the store sees the same state. When that
  process restarts, every key starts up, with no failures counted (NOT-14).
- Inside one program, a name has one breaker, just as it has one `limit`. A
  second open with a different breaker fails with `invalid`, and the error
  shows both.
- Locks have no breaker. Use a semaphore with `limit: 1` instead.
- These are not built yet: waiting for the first free place among several
  semaphores, releasing a place at a point the function chooses before it
  ends, and a budget where one hold weighs more than another.

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
