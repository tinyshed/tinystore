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

How many may hold a key at once, from 1 to 1000.

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

A key of a semaphore, for a hold that says how long it waits.

### SemaphoreKey.wait

```rust
pub fn wait(self, wait: Duration) -> Self
```

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

## Permit

```rust
pub struct Permit<'a> { /* private fields */ }
```

A holding of a semaphore's key: one of its limit.

### Permit.lost

```rust
pub fn lost(&self) -> bool
```

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

<!-- Generated by task reference from crates/tinystore through rustdoc. Edit the doc comments there, not this file. -->
