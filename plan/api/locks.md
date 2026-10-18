# locks: the API book

A key that one holder at a time, or a few, may hold while a function runs,
across every process of a store. Locks live in the memory of the process
that holds the store; kv.db keeps one number of theirs, the epoch their
fences start from. This book is a proposal of 16 October 2026, not yet
accepted: it holds the call sites after a newcomer check and three outside
reviews. [dx.md](../dx.md) holds the rules it follows, and how Python and Go
spell the calls of TypeScript and Rust; research's
[locks round](https://github.com/tinyshed/research/blob/main/tinystore/reports/locks-2026-10-15.md),
its [follow-up on the grouped writer](https://github.com/tinyshed/research/blob/main/tinystore/reports/core-locks-followup-2026-10-15.md)
and the [handoff](https://github.com/tinyshed/research/blob/main/tinystore/reports/channels-locks-handoff-2026-10-15.md)
the evidence and the traps. The Go engine had no locks.

## What a newcomer learns

| Word        | What it is                                                                               |
|-------------|------------------------------------------------------------------------------------------|
| a lock      | a name whose keys are held one holder at a time: `invoices`, and an invoice's id its key |
| a semaphore | the same with up to `limit` holders a key                                                |
| a lease     | a holding of a key, from the moment it is given until it is let go, revoked or lost      |
| a fence     | a number that grows with every lease the store gives, for whoever must refuse a late one |
| waiting     | standing in line for a key, first come first served, `wait` at most                      |

## Hold

```ts
const invoices = store.lock('invoices', { ttl: '10s' })

await invoices.hold(invoice.id, async lease => {
  await charge(invoice, { fence: lease.fence, signal: lease.signal })
})
await invoices.hold(invoice.id, work, { wait: '30s' })

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
invoices, err := locks.OpenLock(store, "invoices", locks.TTL(10*time.Second))   // *locks.Lock

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
let tried: Option<Receipt> = invoices.try_hold(&invoice.id, |lease| work(lease))?;
```

- `hold` waits for the key, holds it, runs the function, and lets the key
  go when the function returns or throws; it answers what the function
  answered.
- Busy keys are waited for in line, `wait` at most, ten seconds unless
  given, and then the call fails `in use`. A program that would wait as
  long as it takes says so with a longer `wait`.
- `tryHold` never waits: `{ acquired: true, value }`, or `{ acquired:
  false }` when the key is held. Rust's `try_hold` answers `None` then,
  `Some` of what the function answered; Python's `try_hold` gives `None`
  for the lease, and Go's `TryHold` `false`, the function not run.
- Python holds through `async with`, as an `asyncio.Lock` is held: the
  block is the function, and a lease lost cancels it at its next `await`,
  as `asyncio.timeout` does, and the block ends `LostError`. Go's function
  gets a `ctx` that ends when the lease is lost, and its error is `hold`'s.
- In Rust the function's error is the caller's own type, which a
  `tinystore` error converts into, as `once` has it.
- A program names keys in one order when it holds two at once: a transfer
  holding `a` then `b` and another holding `b` then `a` wait for each other
  until `wait`.

## Why memory

One process holds a store's directory: its `LOCK` keeps every other out.
Every holder of a lock is a thread of that process or a client that reached
it through its server, so that process sees every lease and every waiter,
and nothing else could. What a lock knows, who holds a key, who waits and
until when, is about work running now, not data: kv's `once` already keeps
the keys running and their waiters this way.

- Giving, renewing and letting go a lease is a change in memory: no
  transaction, no grouped commit, no sync. Research's grouped-writer round
  held a hot key to tens or hundreds of grants a second, each a commit.
- A lease's deadline is taken when it is given, never before a writer was
  waited for: the round found a lease written that way already expired on
  arrival.
- A store that opens again holds no lease: the threads that held them ended
  with the process, and a client that held one through the server learns
  at its next renewal that it lost it. A lease kept on disk would make a
  store that restarted wait out the `ttl` of holders that are gone.

A lock is not a distributed lock: it orders the holders of one store, and a
lease does not outlive the store that gave it.

## The lease

A holder in the store's own process, a Rust thread, cannot die apart from
the store, and its lease ends when its function returns or panics. A holder
through a connection, an SDK in any of its ways, can: its process may freeze
or end while the store goes on, so its lease lasts `ttl` from its last
renewal.

- The SDK renews a lease at a third of `ttl` while the function runs, from
  the holder's own process, so that a program that froze stops renewing.
- A holder counts its lease lost at its own deadline, the time it sent the
  last renewal that succeeded plus `ttl`, less a margin; one renewal that
  failed is not yet a loss. A lease that ran out is never renewed: holding
  the key again is a new lease with a new fence.
- `maxHold`, given when the lock opens, bounds a lease from the moment it
  was given, however it is renewed: past it the store revokes the lease and
  gives the key to the next in line. It is off unless given, for a Rust
  holder and an SDK's alike, since only the program knows how long its work
  may take; a holder that hangs holds its key until it is given.
- A lease lost, run out or revoked, aborts `lease.signal` at that moment,
  and in Rust `lease.lost()` turns true; once the function ends, `hold`
  fails `lost`, even if the function succeeded, so the caller learns that
  another holder may have run beside it. Code that ignores the signal is not
  stopped: the store cannot stop it, and no restart of the store stops a
  call a client already made.
- Letting go checks the lease: one that ran out or was revoked never lets go
  a key another holds now.

## Leases through a connection

- `locks.hold` is a call the server answers once the key is held, with the
  lease's id, its fence and how long until `maxHold` revokes it; a CANCEL
  takes it out of its line. `locks.renew` and `locks.release` are calls of
  their own that name the lease, so that they work from any connection. The
  SDK counts `maxHold` from the answer itself, which no renewal moves, and
  aborts the signal there; the store revokes the lease at the same moment.
- The lease's id is 128 random bits only its holder learns. It, not the
  fence, which the holder hands to others, proves who holds: renewing and
  letting go name it, and a wrong one is refused. A connection admitted to
  write may use it, one that only reads may not.
- A request carries an id of its own, 128 random bits the SDK draws. A
  grant whose answer was lost is asked for again with the same request id,
  on any connection, and the store answers the lease it gave, while that
  lease lives, rather than giving the key twice.
- A connection that drops leaves the lease to its deadline, and the SDK
  renews it by its id on the next connection.
- A store that opened again knows no lease: a renewal of one is `lost`.

## The fence

```ts
await invoices.hold(invoice.id, async lease => {
  await ledger.write(invoice.id, state, { fence: lease.fence })   // the ledger refuses a smaller one
})
```

```python
async with invoices.hold(invoice.id) as lease:
    await ledger.write(invoice.id, state, fence=lease.fence)       # an int of 128 bits
```

```go
err = invoices.Hold(ctx, invoice.ID, func(ctx context.Context, lease *locks.Lease) error {
	return ledger.Write(ctx, invoice.ID, state, lease.Fence) // a locks.Fence, compared by Less
})
```

- A fence is two numbers compared in order, the store's epoch and a count:
  the epoch in the high 64 bits of a 128-bit integer, the count in the low.
  It is a `bigint` in TypeScript, an `int` in Python, a decimal string on
  the wire, an ordered `Fence` in Rust and a `locks.Fence` of two `uint64`s
  in Go.
- The count is the store's, one for every lock and key, and grows with every
  lease it gives while it is open; a key that nobody holds or waits for is
  forgotten, so a count of its own would start again.
- The epoch is written to kv.db each time the store opens, as the larger of
  the time then in milliseconds and the epoch before it plus one.
- A fence protects only where something compares it: a system that keeps
  the largest fence it has seen for a key refuses a late write from a holder
  whose lease ran out while it was paused. A payment provider that compares
  nothing needs an idempotency key all the same; a lock only makes two
  attempts at once rarer.

What a fence promises:

| Between                                            | A later lease's fence is larger                                                     |
|----------------------------------------------------|-------------------------------------------------------------------------------------|
| two leases of one store while it is open           | always                                                                              |
| a store and itself after a crash or a restart      | always: the epoch before it is read from the same file                              |
| a store and one restored from a backup             | only while the restoring machine's clock is past every epoch given after the backup |
| a primary and the replica promoted after it, later | as a restore, until replication's arbiter gives terms that only grow                |

A restore brings back the epoch of its backup's moment, so the epoch after
it is larger than those given since only while the clock is: a clock set a
day back and a backup restored gives fences smaller than ones already given.
That is the assumption `ttl` makes too, and the book promises no more. A
strict order across a restore needs a source of epochs that a backup does
not carry, which replication's lease in object storage will be.

## Semaphores

```ts
const uploads = store.semaphore('uploads', { limit: 3, ttl: '30s' })
await uploads.hold(user.id, () => upload(file))          // three uploads a user at once
```

```python
uploads = store.semaphore("uploads", limit=3, ttl="30s")
async with uploads.hold(user.id):
    await upload(file)
```

```go
uploads, err := locks.OpenSemaphore(store, "uploads", locks.Limit(3), locks.TTL(30*time.Second))
err = uploads.Hold(ctx, user.ID, func(ctx context.Context, _ *locks.Lease) error {
	return upload(ctx, file)
})
```

```rust
let uploads = store.semaphore("uploads").limit(3).open()?;
uploads.hold(&user.id, |_| upload(&file))?;
```

- A semaphore is a lock whose key `limit` holders may hold at once, with
  the same `hold`, `tryHold`, lease, `maxHold`, signal and `lost`.
- Its lease has no fence: three holders are valid at once, and a system
  that refused all but the largest fence would refuse two of them.
- Two opens of a name with different limits: the key admits by the limit of
  the open that waits, and a smaller one lets no holder go, it only lets
  fewer in.

## Where `hold` may not be called

- Inside a transaction, `store.tx` or `db.tx`: `invalid`, naming the lock.
  The transaction would hold its file's writer while the key is waited for.
  Hold first, then begin the transaction.
- Inside a hold of the same key, in the same async chain or thread:
  `invalid` at once. The inner call would wait for the outer, which never
  lets go while it waits.

## Errors

| Kind      | When                                                                                       |
|-----------|--------------------------------------------------------------------------------------------|
| `invalid` | a bad name or key; `hold` inside a transaction or inside a hold of the same key            |
| `in use`  | `wait` passed with the key still held                                                      |
| `lost`    | the lease ran out, was revoked at `maxHold`, or was given by a store that has since closed |
| `limit`   | the line of waiters for a key, or the store's memory, is full                              |
| `closed`  | the store closed: the signal aborts, and every lease of the store ends                     |

`lost` is a kind of its own, the one locks add to `ErrorKind`. Every error
names the lock and the key: `lock invoices: key "42": lost`.

## Bounds

Proposed, to be measured before they are promised.

| What                  | Bound                     |
|-----------------------|---------------------------|
| a name                | 64 bytes                  |
| a key                 | 1 KiB                     |
| `ttl`                 | 1 s to 1 hour, 10 s unset |
| `wait`                | 10 s unset                |
| a semaphore's `limit` | 1 to 1000                 |
| waiters for one key   | 10,000                    |

## Under the surface

- A map of keys held or waited for, each with its holders, their lease's id,
  fence, deadline and `maxHold`'s, and its line of waiters. A key with
  neither leaves the map.
- A waiter is a callback, as `once`'s are, not a blocked thread: a client
  on an event loop waits without one of the store's threads. A key let go is
  given at once to the first in line, and only that one is woken. A waiter
  that leaves, its call cancelled or its connection closed, leaves the line.
- One timer of the store's wakes at the earliest of what is due: a lease's
  `maxHold`, the deadline of a lease someone waits behind, a waiter's
  `wait`. It revokes, gives on or gives up, and sets itself again. A lease
  that ran out with nobody behind it is found out when someone asks.
- The epoch is a row of kv's own, so `locks` turns kv on; the count is an
  integer in memory.
- jobs' `concurrency.group` is not a lock: a job's slot is taken in the same
  transaction as the job, in the queue's file, and stays jobs' as it is.

## Slices

1. The Rust core: locks and semaphores in memory, the epoch in kv.db,
   fences, the line of waiters and the timer, `hold`, `try_hold`, `maxHold`,
   `lost`, the two places `hold` is refused.
2. The wire and the Bun SDK: `locks.hold`, `locks.renew` and
   `locks.release`, renewals from the SDK, the lease's and the request's ids
   across connections, `lease.signal`.

Python and Go come with their bindings.

## Gates

- Random histories of holds, lettings go, cancellations, revocations and
  clock moves never leave two holders of a lock's key, or more than `limit`
  of a semaphore's.
- A Rust holder that hangs keeps its key without `maxHold` and is revoked
  at it, the next in line served and the hung one told `lost`.
- A grant whose answer was dropped, asked again by its request id on
  another connection, answers the same lease; a renewal with a wrong lease
  id is refused.
- A connection dropped and made again renews its lease within its deadline;
  past it the lease is lost and the key given on.
- A store that opens again knows no lease, and its next fence is larger
  than every fence of its last life.
- Waiters are served in line, a cancelled one leaves it, and only the next
  is woken.
- A fence handed to TypeScript keeps every digit.

## Later

- Holding several keys at once, sorted by the store: `hold([a, b], fn)`.
- `acquire`, `renew` and `release` for a holder that outlives a function:
  adding them later breaks nothing, and removing them would.
- A bucket that refuses a write with a stale fence, `ledger.set(id, state,
  { fence })`, so that a fence protects inside the store too.
- Epochs from replication's arbiter, which order fences across a restore and
  a failover.
- Leases kept on disk, should a lease ever need to outlive a restart of its
  server.

## Newcomer check and reviews, 16 October

Three Haiku agents that had read nothing about TinyStore read a draft at the
highest effort: a newcomer's reading of fifteen bare calls, a free review,
and a blind choice between spellings. An outside reviewer read it before and
after them, and again after research's follow-up, when locks moved to memory.

| It read                                                             | Change                                                        |
|---------------------------------------------------------------------|---------------------------------------------------------------|
| `invoices.with(key, fn)` beside `tx.with(bucket)`                   | `hold`                                                        |
| `acquire(key, { wait: 0 })`, a number beside `wait: '2s'`           | `tryHold`, which never waits                                  |
| `tryHold` answering `undefined` when busy and when `fn` returned it | `{ acquired, value }`                                         |
| `permits` on a lock, with a fence three holders share               | `store.semaphore(name, { limit })`, its lease without a fence |
| a lease lost only through `signal`                                  | `hold` fails `lost` too                                       |
| a holder alive but stuck, renewing forever                          | `maxHold`, from the grant, for every holder                   |
| `with` waiting without end when no `wait` is given                  | ten seconds, then `in use`                                    |
| `hold` inside a transaction and inside a hold of the same key       | `invalid` at once                                             |
| `acquire`, `renew`, `release` beside `hold`                         | left for later                                                |
| a grant written to kv.db, its deadline taken before the writer      | leases in memory, the deadline taken as the lease is given    |
| the fence surviving a restore through an epoch the backup holds     | an epoch from the clock, its promise in the table above       |
| a fence as the epoch times a count                                  | two numbers in one 128-bit integer, compared in order         |
| the request's id proving who holds                                  | a lease id only its holder knows                              |
| `wait` tied to `ttl`                                                | its own default                                               |

The newcomer read `store.lock` as a verb and offered `store.locks`; the
others did not, and `lock` stands beside `bucket`, `queue` and `channel`.
The newcomer offered `timeout` for `wait`, which would read as the
function's bound. The reviewers read `with` as a reserved word of
JavaScript; as a property it is not, and `hold` is chosen for its meaning.
