# Locks API for Bun and Node

Every public class, function and type of the Locks engine in `@tinyshed/tinystore`, generated from its source. The [Locks guide](../../guides/locks/README.md) explains how to use them, and the [Python](../python/locks.md), [Go](../go/locks.md) and [Rust](../rust/locks.md) pages list the same API.

## LockOptions

```ts
interface LockOptions {
    /** how long a lease lasts from its last renewal: 10 s unless said, 1 s to 1 hour */
    ttl?: Duration | undefined
    /** how long a lease lasts from its grant, however it is renewed: past it the next in line holds the key */
    maxHold?: Duration | undefined
}
```

## SemaphoreOptions

```ts
interface SemaphoreOptions extends LockOptions {
    /** how many may hold a key at once, 1 to 1000 */
    limit: number
    /** how many of a key's places background holds may take at once, 1 to limit: every place unless said */
    background?: number | undefined
    /** puts a key down for `pause` once `failures` holds of it failed in a row: every ask is then UnavailableError at once */
    breaker?: Breaker | undefined
}
```

## Breaker

```ts
interface Breaker {
    /** failed holds in a row, 1 or more */
    failures: number
    pause: Duration
}
```

A semaphore's breaker. A hold failed when its function threw, unless its
permit said `healthy()`, or when it ran past its timeout or maxHold; a
success resets the count. Once the pause is over, one hold goes through:
its success puts the key up, its failure pauses it again. The store's
process keeps it in memory, so a restart of that process puts every key up.

## KeyState

```ts
interface KeyState {
    /** false from the failure that put the key down until a hold after the pause succeeds; true without a breaker */
    up: boolean
    downSince?: Date | undefined
    /** the last failed hold's end, kept until a success after it */
    lastFailure?: Date | undefined
    waiting: number
    holding: number
    /**
     * the failed holds in a row the breaker counts now, toward its `failures`:
     * reset by a success, and by a pause with no failure; while the key is
     * down, those that put it down; 0 without a breaker
     */
    failures: number
}
```

How a semaphore's key stands now: `semaphore.state(key)`.

## HoldOptions

```ts
interface HoldOptions {
    /** how long the hold waits in line: 10 s unless said, then InUseError */
    wait?: Duration | undefined
}
```

## Priority

```ts
type Priority = 'urgent' | 'background'
```

Which line a semaphore's hold waits in: urgent holds are given a place
before background ones, and a background hold waiting never keeps an urgent
one from a free place. A background hold is promised no place: it waits
while urgent holds keep coming, `wait` at most.

## PermitOptions

```ts
interface PermitOptions {
    /** urgent unless said */
    priority?: Priority | undefined
    /** how long the hold may keep its place, from its grant: past it the next in line holds it and the hold is LostError */
    timeout?: Duration | undefined
}
```

## SemaphoreHoldOptions

```ts
interface SemaphoreHoldOptions extends HoldOptions, PermitOptions {
}
```

## Tried

```ts
type Tried<T> = {
    acquired: true
    value: T
} | {
    acquired: false
}
```

What tryHold gives: the function's value, or that the key was held.

## Lease

```ts
interface Lease {
    /** grows with every lease the store gives: hand it to whatever must refuse a late holder */
    readonly fence: bigint
    /** aborts when the lease is lost: run out, revoked at maxHold, or its store closed */
    readonly signal: AbortSignal
    /** how long the hold waited for the key, from its call to the grant, the round trip included */
    readonly waited: DurationText
}
```

A holding of a lock's key, given to the function that holds it.

## Permit

```ts
interface Permit {
    readonly signal: AbortSignal
    /** how long the hold waited for its place, from its call to the grant, the round trip included */
    readonly waited: DurationText
    /** says the guarded service is healthy, so that the breaker does not count the hold a failure though fn then throws: a 404 that is the caller's */
    healthy(): void
}
```

A holding of a semaphore's key: one of its limit.

## Lock

```ts
class Lock {
    readonly name: string
}
```

Keys held one holder at a time: open them with `store.lock(name)`.

### Lock.hold

```ts
hold<T>(key: Key, fn: (lease: Lease) => T | Promise<T>, options?: HoldOptions): Promise<T>
```

Waits for key, holds it while fn runs and lets it go, giving what fn
gives. A key still held after the wait is InUseError; a lease lost
while fn ran is LostError, though fn succeeded.

    await invoices.hold(invoice.id, lease => charge(invoice, { fence: lease.fence, signal: lease.signal }))

### Lock.tryHold

```ts
tryHold<T>(key: Key, fn: (lease: Lease) => T | Promise<T>): Promise<Tried<T>>
```

Holds key while fn runs if nobody holds it, and says it was held without waiting if somebody does.

## Semaphore

```ts
class Semaphore {
    readonly name: string
    readonly limit: number
}
```

Keys held by up to their limit at a time: open them with `store.semaphore(name, { limit })`.

### Semaphore.hold

```ts
hold<T>(key: Key, fn: (permit: Permit) => T | Promise<T>, options?: SemaphoreHoldOptions): Promise<T>
```

Waits for one of key's places, holds it while fn runs and lets it go,
as a lock's hold does; in the background, it waits behind every urgent
hold and takes no more than the semaphore's `background` places:

    await cabinet.hold('requests', permit => sync(permit.signal), { priority: 'background', timeout: '15s' })

### Semaphore.tryHold

```ts
tryHold<T>(key: Key, fn: (permit: Permit) => T | Promise<T>, options?: PermitOptions): Promise<Tried<T>>
```

### Semaphore.state

```ts
state(key: Key): Promise<KeyState>
```

How key stands now: whether what it guards is up, since when it is
down, its last failed hold, and how many wait for it and hold it.

    const { up, downSince } = await cabinet.state('requests')

<!-- Generated by task reference from sdk/js/src/locks.ts. Edit the doc comments there, not this file. -->
