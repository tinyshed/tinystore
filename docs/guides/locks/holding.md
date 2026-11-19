# Holding and waiting

`hold` waits for a key, holds it while your function runs, and lets it go when
the function returns or throws. Waiters stand in line, first come first
served. `tryHold` never waits: it runs the function only if the key is free.

```ts
const invoices = store.lock('invoices', { ttl: '10s' })

await invoices.hold(invoice.id, async lease => {
	await charge(invoice, { fence: lease.fence, signal: lease.signal })
})
await invoices.hold(invoice.id, work, { wait: '30s' }) // stand in line up to 30 seconds

const tried = await invoices.tryHold(invoice.id, work)
if (tried.acquired) use(tried.value)
else say('someone is charging it already')
```

```python
invoices = store.lock("invoices", ttl="10s")

async with invoices.hold(invoice.id) as lease:
    await charge(invoice, fence=lease.fence)
async with invoices.hold(invoice.id, wait="30s") as lease:
    await work(lease)

async with invoices.try_hold(invoice.id) as lease:
    if lease is None:
        say("someone is charging it already")
    else:
        use(await work(lease))
```

```go
invoices, err := locks.OpenLock(store, "invoices", locks.TTL(10*time.Second))

err = invoices.Hold(ctx, invoice.ID, func(ctx context.Context, lease *locks.Lease) error {
	return charge(ctx, invoice, lease.Fence) // ctx ends when the lease is lost
})
err = invoices.Hold(ctx, invoice.ID, work, locks.Wait(30*time.Second))

acquired, err := invoices.TryHold(ctx, invoice.ID, work)
if err == nil && !acquired {
	say("someone is charging it already")
}
```

```rust
let invoices = store.lock("invoices").open()?;

invoices.hold(&invoice.id, |lease| charge(&invoice, lease.fence()))?;
invoices.key(&invoice.id).wait(Duration::from_secs(30)).hold(|lease| work(lease))?;
let tried: Option<Receipt> = invoices.try_hold(&invoice.id, |lease| work(lease))?; // None when held
```

| Call      | Key free          | Key held                                     |
|-----------|-------------------|----------------------------------------------|
| `hold`    | runs the function | waits in line, `wait` at most, then `in use` |
| `tryHold` | runs the function | `{ acquired: false }`, the function not run  |

`hold` answers what the function answered. `wait` is ten seconds unless
given. A program that would wait as long as it takes says so with a longer
one.

## How long a hold waited

A lease and a permit say how long their hold waited, from the call of `hold`
to the moment the key was given. It is never less than the time the hold
stood in line (contract LOCK-08). Through a connection, the core in your
process included, it also holds the round trip.

```ts
const moodle = store.semaphore('moodle', { limit: 24 })

await moodle.hold('requests', async permit => {
	queueWait.record(permit.waited) // '22ms'
	return fetchMoodle(permit.signal)
})
```

```python
moodle = store.semaphore("moodle", limit=24)

async with moodle.hold("requests") as permit:
    queue_wait.record(permit.waited)  # timedelta(microseconds=22000)
    await fetch_moodle()
```

```go
moodle, err := locks.OpenSemaphore(store, "moodle", locks.Limit(24))

err = moodle.Hold(ctx, "requests", func(ctx context.Context, permit *locks.Lease) error {
	queueWait.Record(permit.Waited) // 22ms, a time.Duration
	return fetchMoodle(ctx)
})
```

```rust
let moodle = store.semaphore("moodle").limit(24).open()?;

moodle.hold("requests", |permit| {
    queue_wait.record(permit.waited()); // a Duration
    fetch_moodle()
})?;
```

In TypeScript `waited` is a duration's text, as every duration the SDK gives
back; `ms(permit.waited)` is its milliseconds. A `tryHold` that ran its
function waited only the round trip.

With the store's [own metrics](../metrics/self-metrics.md) on, the store
times every wait in `tinystore_locks_wait_ms` and says how many asks wait and
how many leases are held now, by the lock's or semaphore's name.

## One key, one holder

A key has one holder at a time (contract LOCK-01). The key is any text or
number: an invoice's id, a chat's. Keys of one lock are independent: holding
invoice 42 never makes invoice 43 wait.

```text
lock invoices: key "9": held past the hold's wait: in use
```

## Where `hold` may not be called

| Inside                               | Why it is `invalid`                                                                                 |
|--------------------------------------|-----------------------------------------------------------------------------------------------------|
| a transaction, `store.tx` or `db.tx` | the transaction would hold its file's writer while the key is waited for: hold first, then begin it |
| a hold of the same key               | the inner call would wait for the outer, which never lets go                                        |

Both fail at once, naming the lock and the key, rather than waiting until
`wait` (contract LOCK-04).

A program that holds two keys at once names them in one order: a transfer
holding `a` then `b`, and another holding `b` then `a`, wait for each other
until `wait`. Sort the keys first.

## `maxHold`

A holder can hang: a call to a payment provider that never answers. Its lease
is renewed while its process lives, so its key stays held. `maxHold` bounds a
lease from the moment it was given, however it is renewed:

```ts
const invoices = store.lock('invoices', { ttl: '10s', maxHold: '2m' })
```

```python
invoices = store.lock("invoices", ttl="10s", max_hold="2m")
```

```go
invoices, err := locks.OpenLock(store, "invoices", locks.TTL(10*time.Second), locks.MaxHold(2*time.Minute))
```

```rust
let invoices = store.lock("invoices").max_hold(Duration::from_secs(120)).open()?;
```

Past it, the store revokes the lease, gives the key to the next in line, and
`hold` fails `lost` (contract LOCK-03):

```text
lock invoices: key "m": the lease was lost: it was revoked at its maxHold
```

[When a lease is lost](leases.md#when-a-lease-is-lost) says how your function
hears it.

`maxHold` is off unless given, because only the program knows how long its
work may take.

## Locks live in memory

A store's locks live in the memory of the process that holds it, so they
order the holders of one store and do not coordinate two stores on two
machines (NOT-03). Every holder is a thread of that process or a client of
its server, so that process sees every lease and every waiter. Giving and
letting go a key is a change in memory: no write, no sync. A store that opens
again holds no lease.

## Errors

| Kind      | When                                                                                                                                                                                         |
|-----------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `invalid` | a bad name or key; `hold` inside a transaction or inside a hold of the same key; a semaphore opened again in the program with [another limit](semaphores.md#two-opens-with-different-limits) |
| `in use`  | `wait` passed with the key still held                                                                                                                                                        |
| `lost`    | the lease ran out, was revoked at `maxHold`, or its store closed                                                                                                                             |
| `limit`   | the line of waiters for a key is full, 10,000                                                                                                                                                |
| `closed`  | the store closed                                                                                                                                                                             |

## See also

- [Locks](README.md): the engine's overview.
- [Leases over a connection](leases.md): `ttl`, renewal and a lost lease.
- [Fences](fences.md): refusing a write from a holder that stalled.
