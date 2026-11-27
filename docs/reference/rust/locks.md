# Locks API for Rust

Every public type, trait and function of the Locks engine in `tinystore::locks` and `tinystore::aio::locks`, generated from its source. The [Locks guide](../../guides/locks/README.md) explains how to use them, and the [Bun and Node](../bun/locks.md), [Python](../python/locks.md) and [Go](../go/locks.md) pages list the same API.

## Store

```rust
impl Store
```

### Store.lock

```rust
pub fn lock(&self, name: &str) -> LockBuilder
```

Keys held one holder at a time, across every process of the store:
`hold` waits for a key, runs a function and lets the key go.

### Store.semaphore

```rust
pub fn semaphore(&self, name: &str) -> SemaphoreBuilder
```

Keys held by up to `limit` holders at a time.

## aio::Store

```rust
impl aio::Store
```

### aio::Store.lock

```rust
pub fn lock(&self, name: &str) -> LockBuilder
```

Keys held one holder at a time, across every process of the store, as
`crate::Store::lock`: a hold is awaited.

### aio::Store.semaphore

```rust
pub fn semaphore(&self, name: &str) -> SemaphoreBuilder
```

Keys held by up to `limit` holders at a time, as
`crate::Store::semaphore`.

## KeyState

```rust
pub struct KeyState {
    pub up: bool,
    pub down_since: Option<SystemTime>,
    pub last_failure: Option<SystemTime>,
    pub waiting: u32,
    pub holding: u32,
}
```

How a semaphore's key stands now: whether what it guards is up, and how
many wait for it and hold it.

## Fence

```rust
pub struct Fence(_)
```

A number that grows with every lease a store gives, so that whoever keeps
the largest it has seen can refuse a late holder: the store's epoch in the
high 64 bits, which grows each time the locks of a store open, and a count
of its leases since in the low 64.

### Fence.get

```rust
pub fn get(self) -> u128
```

The fence as one integer, compared as it is.

## LockBuilder

```rust
pub struct LockBuilder { /* private fields */ }
```

A lock being opened.

### LockBuilder.max_hold

```rust
pub fn max_hold(self, bound: Duration) -> Self
```

How long one lease may last from its grant: past it the key is given
to the next in line and the holder told it lost it. Off unless given.

### LockBuilder.open

```rust
pub fn open(self) -> Result<Lock>
```

## SemaphoreBuilder

```rust
pub struct SemaphoreBuilder { /* private fields */ }
```

A semaphore being opened.

### SemaphoreBuilder.limit

```rust
pub fn limit(self, limit: u32) -> Self
```

How many may hold a key at once, from 1 to 1000. A name has one limit
in a program: an open with another limit than the name's first open
in the program gave is `Invalid`. Another program's holds, a client's
over a connection, are each admitted by their own limit.

### SemaphoreBuilder.background

```rust
pub fn background(self, places: u32) -> Self
```

How many of a key's places background holds may take at once, from 1
to the limit, so that urgent holds always find the rest; background
holds may take every place unless given. A name keeps one in a
program, as it keeps its limit.

### SemaphoreBuilder.breaker

```rust
pub fn breaker(self, failures: u32, pause: Duration) -> Self
```

Puts a key down for `pause` once `failures` holds of it failed in a
row: every ask of it, waiting or new, is then refused at once
`Unavailable`. When the pause is over, one ask goes through. Its
success puts the key up again, and its failure pauses it again.

A hold failed when its work answered an error, unless its permit said
`Permit::succeeded`, or when it ran past its timeout or `max_hold`. A
success resets the count. The breaker lives in the memory of the
process that holds the store, beside the lines: every program of the
store sees one answer, and a restart of that process starts every key
up. A name keeps one breaker in a program, as it keeps its limit.

### SemaphoreBuilder.max_hold

```rust
pub fn max_hold(self, bound: Duration) -> Self
```

### SemaphoreBuilder.open

```rust
pub fn open(self) -> Result<Semaphore>
```

## Lock

```rust
pub struct Lock { /* private fields */ }
```

A lock's keys, each held by one holder at a time.

### Lock.hold

```rust
pub fn hold<T, E: From<Error>>(
    &self,
    key: impl Key,
    work: impl FnOnce(&Lease<'_>) -> Result<T, E>,
) -> Result<T, E>
```

Waits for `key`, ten seconds at most, holds it while `work` runs and
lets it go, answering what `work` answered. A key still held after the
wait is `in use`; a lease lost while `work` ran is `lost`, even when
`work` succeeded.

### Lock.try_hold

```rust
pub fn try_hold<T, E: From<Error>>(
    &self,
    key: impl Key,
    work: impl FnOnce(&Lease<'_>) -> Result<T, E>,
) -> Result<Option<T>, E>
```

Holds `key` while `work` runs if nobody holds it, and answers `None`
without waiting if somebody does.

### Lock.key

```rust
pub fn key(&self, key: impl Key) -> LockKey<'_>
```

A key, for a hold that says how long it waits.

## Semaphore

```rust
pub struct Semaphore { /* private fields */ }
```

A semaphore's keys, each held by up to its limit at a time.

### Semaphore.hold

```rust
pub fn hold<T, E: From<Error>>(
    &self,
    key: impl Key,
    work: impl FnOnce(&Permit<'_>) -> Result<T, E>,
) -> Result<T, E>
```

### Semaphore.try_hold

```rust
pub fn try_hold<T, E: From<Error>>(
    &self,
    key: impl Key,
    work: impl FnOnce(&Permit<'_>) -> Result<T, E>,
) -> Result<Option<T>, E>
```

### Semaphore.key

```rust
pub fn key(&self, key: impl Key) -> SemaphoreKey<'_>
```

A key, for a hold that says how long it waits, whether it waits in
the background and how long it may keep its place.

### Semaphore.state

```rust
pub fn state(&self, key: impl Key) -> Result<KeyState>
```

How `key` stands now: up or down and since when, its last failed
hold, and how many wait for it and hold it. Up always without a
breaker.

## LockKey

```rust
pub struct LockKey<'a> { /* private fields */ }
```

A key of a lock, for a hold that says how long it waits.

### LockKey.wait

```rust
pub fn wait(self, wait: Duration) -> Self
```

How long the hold waits in line; ten seconds unless said.

### LockKey.hold

```rust
pub fn hold<T, E: From<Error>>(self, work: impl FnOnce(&Lease<'_>) -> Result<T, E>) -> Result<T, E>
```

### LockKey.try_hold

```rust
pub fn try_hold<T, E: From<Error>>(
    self,
    work: impl FnOnce(&Lease<'_>) -> Result<T, E>,
) -> Result<Option<T>, E>
```

## SemaphoreKey

```rust
pub struct SemaphoreKey<'a> { /* private fields */ }
```

A key of a semaphore, for a hold that says how long it waits, at which
priority, and how long it may keep its place.

### SemaphoreKey.wait

```rust
pub fn wait(self, wait: Duration) -> Self
```

### SemaphoreKey.background

```rust
pub fn background(self) -> Self
```

Waits behind every urgent hold of the key, and takes no more of its
places than the semaphore's `background`. Nothing promises it a place:
while urgent holds keep coming and take the room, it waits, `wait` at
most.

### SemaphoreKey.timeout

```rust
pub fn timeout(self, timeout: Duration) -> Self
```

How long the hold may keep its place, from its grant: past it the
place goes to the next in line and the hold fails `lost`, as at the
semaphore's `max_hold`, the shorter of the two counting.

### SemaphoreKey.hold

```rust
pub fn hold<T, E: From<Error>>(self, work: impl FnOnce(&Permit<'_>) -> Result<T, E>) -> Result<T, E>
```

### SemaphoreKey.try_hold

```rust
pub fn try_hold<T, E: From<Error>>(
    self,
    work: impl FnOnce(&Permit<'_>) -> Result<T, E>,
) -> Result<Option<T>, E>
```

## Lease

```rust
pub struct Lease<'a> { /* private fields */ }
```

A holding of a lock's key, given to the function that holds it.

### Lease.fence

```rust
pub fn fence(&self) -> Fence
```

The lease's fence: hand it to whatever must refuse a late holder.

### Lease.lost

```rust
pub fn lost(&self) -> bool
```

Whether the lease ended while its function runs: revoked at its
`max_hold`, or the store closed. What the function does from then on
may run beside another holder.

### Lease.waited

```rust
pub fn waited(&self) -> Duration
```

How long the hold waited for the key, from its call to the grant.

## Permit

```rust
pub struct Permit<'a> { /* private fields */ }
```

A holding of a semaphore's key: one of its limit.

### Permit.lost

```rust
pub fn lost(&self) -> bool
```

### Permit.succeeded

```rust
pub fn succeeded(&self)
```

Says the hold succeeded, so that the semaphore's breaker does not count
it a failure though its work then answers an error: a 404 that is the
caller's, not the service's.

### Permit.waited

```rust
pub fn waited(&self) -> Duration
```

How long the hold waited for its place, from its call to the grant.

## aio::locks::LockBuilder

```rust
pub struct LockBuilder { /* private fields */ }
```

A lock being opened, as `crate::locks::LockBuilder`.

### aio::locks::LockBuilder.max_hold

```rust
pub fn max_hold(self, bound: Duration) -> Self
```

How long one lease may last from its grant: past it the key is given
to the next in line and the hold answers `lost`. Off unless given.

### aio::locks::LockBuilder.open

```rust
pub fn open(self) -> Result<Lock>
```

Opens the lock: a plain call, made once at start. The first lock or
semaphore of a store writes the locks' epoch to kv.db.

## aio::locks::SemaphoreBuilder

```rust
pub struct SemaphoreBuilder { /* private fields */ }
```

A semaphore being opened, as `crate::locks::SemaphoreBuilder`.

### aio::locks::SemaphoreBuilder.limit

```rust
pub fn limit(self, limit: u32) -> Self
```

How many may hold a key at once, from 1 to 1000.

### aio::locks::SemaphoreBuilder.background

```rust
pub fn background(self, places: u32) -> Self
```

How many of a key's places background holds may take at once, as
`crate::locks::SemaphoreBuilder::background`.

### aio::locks::SemaphoreBuilder.breaker

```rust
pub fn breaker(self, failures: u32, pause: Duration) -> Self
```

Puts a key down for `pause` once `failures` holds of it failed in a
row, as `crate::locks::SemaphoreBuilder::breaker`: a hold whose
future answered an error failed, and one whose future was dropped
counts neither way.

### aio::locks::SemaphoreBuilder.max_hold

```rust
pub fn max_hold(self, bound: Duration) -> Self
```

### aio::locks::SemaphoreBuilder.open

```rust
pub fn open(self) -> Result<Semaphore>
```

Opens the semaphore: a plain call, made once at start.

## aio::locks::Lock

```rust
pub struct Lock { /* private fields */ }
```

A lock's keys, each held by one holder at a time, as
`crate::locks::Lock`.

### aio::locks::Lock.hold

```rust
pub fn hold<'w, T, E, F>(
    &self,
    key: impl Key,
    work: impl FnOnce(Lease) -> F + Send + 'w,
) -> impl Future<Output = Result<T, E>> + Send + 'w
where
    F: Future<Output = Result<T, E>> + Send + 'w,
    T: Send + 'w,
    E: From<Error> + Send + 'w,
```

Waits for `key`, ten seconds at most, holds it while the future `work`
makes runs and lets it go, answering what that future answered. A key
still held after the wait is `in use`; a lease lost meanwhile is
`lost`, even when `work` succeeded.

The future is as `'static` as `work` and what it borrows, so a hold
whose `work` owns what it uses is spawned.

### aio::locks::Lock.try_hold

```rust
pub fn try_hold<'w, T, E, F>(
    &self,
    key: impl Key,
    work: impl FnOnce(Lease) -> F + Send + 'w,
) -> impl Future<Output = Result<Option<T>, E>> + Send + 'w
where
    F: Future<Output = Result<T, E>> + Send + 'w,
    T: Send + 'w,
    E: From<Error> + Send + 'w,
```

Holds `key` while `work` runs if nobody holds it, and answers `None`
without waiting if somebody does.

### aio::locks::Lock.key

```rust
pub fn key(&self, key: impl Key) -> LockKey<'_>
```

A key, for a hold that says how long it waits.

### aio::locks::Lock.blocking

```rust
pub fn blocking(&self) -> &Lock
```

The same lock, for its blocking calls.

## aio::locks::Semaphore

```rust
pub struct Semaphore { /* private fields */ }
```

A semaphore's keys, each held by up to its limit at a time, as
`crate::locks::Semaphore`.

### aio::locks::Semaphore.hold

```rust
pub fn hold<'w, T, E, F>(
    &self,
    key: impl Key,
    work: impl FnOnce(Permit) -> F + Send + 'w,
) -> impl Future<Output = Result<T, E>> + Send + 'w
where
    F: Future<Output = Result<T, E>> + Send + 'w,
    T: Send + 'w,
    E: From<Error> + Send + 'w,
```

As `Lock::hold`, one of the key's `limit` holders.

### aio::locks::Semaphore.try_hold

```rust
pub fn try_hold<'w, T, E, F>(
    &self,
    key: impl Key,
    work: impl FnOnce(Permit) -> F + Send + 'w,
) -> impl Future<Output = Result<Option<T>, E>> + Send + 'w
where
    F: Future<Output = Result<T, E>> + Send + 'w,
    T: Send + 'w,
    E: From<Error> + Send + 'w,
```

As `Lock::try_hold`: `None` at once while `limit` hold the key.

### aio::locks::Semaphore.key

```rust
pub fn key(&self, key: impl Key) -> SemaphoreKey<'_>
```

### aio::locks::Semaphore.state

```rust
pub fn state(&self, key: impl Key) -> Result<KeyState>
```

How `key` stands now, as `crate::locks::Semaphore::state`: a plain
call, since the store's process keeps it in memory.

### aio::locks::Semaphore.blocking

```rust
pub fn blocking(&self) -> &Semaphore
```

The same semaphore, for its blocking calls.

## aio::locks::LockKey

```rust
pub struct LockKey<'a> { /* private fields */ }
```

A key of a lock, for a hold that says how long it waits.

### aio::locks::LockKey.wait

```rust
pub fn wait(self, wait: Duration) -> Self
```

How long the hold waits in line; ten seconds unless said. The wait
ends on the locks' timer, with no thread parked.

### aio::locks::LockKey.hold

```rust
pub fn hold<'w, T, E, F>(
    self,
    work: impl FnOnce(Lease) -> F + Send + 'w,
) -> impl Future<Output = Result<T, E>> + Send + 'w
where
    F: Future<Output = Result<T, E>> + Send + 'w,
    T: Send + 'w,
    E: From<Error> + Send + 'w,
```

As `Lock::hold`.

### aio::locks::LockKey.try_hold

```rust
pub fn try_hold<'w, T, E, F>(
    self,
    work: impl FnOnce(Lease) -> F + Send + 'w,
) -> impl Future<Output = Result<Option<T>, E>> + Send + 'w
where
    F: Future<Output = Result<T, E>> + Send + 'w,
    T: Send + 'w,
    E: From<Error> + Send + 'w,
```

As `Lock::try_hold`.

## aio::locks::SemaphoreKey

```rust
pub struct SemaphoreKey<'a> { /* private fields */ }
```

A key of a semaphore, for a hold that says how long it waits.

### aio::locks::SemaphoreKey.wait

```rust
pub fn wait(self, wait: Duration) -> Self
```

How long the hold waits in line; ten seconds unless said.

### aio::locks::SemaphoreKey.background

```rust
pub fn background(self) -> Self
```

Waits behind every urgent hold of the key, as
`crate::locks::SemaphoreKey::background`: nothing promises it a place
while urgent holds keep coming.

### aio::locks::SemaphoreKey.timeout

```rust
pub fn timeout(self, timeout: Duration) -> Self
```

How long the hold may keep its place from its grant, as
`crate::locks::SemaphoreKey::timeout`: past it the hold answers
`lost`.

### aio::locks::SemaphoreKey.hold

```rust
pub fn hold<'w, T, E, F>(
    self,
    work: impl FnOnce(Permit) -> F + Send + 'w,
) -> impl Future<Output = Result<T, E>> + Send + 'w
where
    F: Future<Output = Result<T, E>> + Send + 'w,
    T: Send + 'w,
    E: From<Error> + Send + 'w,
```

As `Semaphore::hold`.

### aio::locks::SemaphoreKey.try_hold

```rust
pub fn try_hold<'w, T, E, F>(
    self,
    work: impl FnOnce(Permit) -> F + Send + 'w,
) -> impl Future<Output = Result<Option<T>, E>> + Send + 'w
where
    F: Future<Output = Result<T, E>> + Send + 'w,
    T: Send + 'w,
    E: From<Error> + Send + 'w,
```

As `Semaphore::try_hold`.

## aio::locks::Lease

```rust
pub struct Lease { /* private fields */ }
```

A holding of a lock's key, given to the function that holds it, and owned
by it: the future it makes may keep it across its awaits.

### aio::locks::Lease.fence

```rust
pub fn fence(&self) -> Fence
```

The lease's fence: hand it to whatever must refuse a late holder.

### aio::locks::Lease.lost

```rust
pub fn lost(&self) -> bool
```

Whether the lease ended: revoked at its `max_hold`, the store closed,
or, for a lease kept past its hold, the hold over. What is done with
it from then on may run beside another holder.

### aio::locks::Lease.waited

```rust
pub fn waited(&self) -> Duration
```

How long the hold waited for the key, from its first poll to the grant.

## aio::locks::Permit

```rust
pub struct Permit { /* private fields */ }
```

A holding of a semaphore's key, one of its limit, owned by the function
that holds it.

### aio::locks::Permit.lost

```rust
pub fn lost(&self) -> bool
```

As `Lease::lost`: true too once its hold is over.

### aio::locks::Permit.succeeded

```rust
pub fn succeeded(&self)
```

As `crate::locks::Permit::succeeded`: the breaker does not count the
hold a failure though its future then answers an error.

### aio::locks::Permit.waited

```rust
pub fn waited(&self) -> Duration
```

As `Lease::waited`.

<!-- Generated by task reference from crates/tinystore through rustdoc. Edit the doc comments there, not this file. -->
