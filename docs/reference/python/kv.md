# KV API for Python

Every public class, function and type of the KV engine in `tinystore`, generated from its source. The [KV guide](../../kv/README.md) explains how to use them, and the [Bun and Node](../bun/kv.md) and [Go](../go/kv.md) pages list the same API.

## Entry

```python
@dataclass(frozen=True, slots=True)
class Entry[T]:
    key: str
    value: T
    version: Version  # compared only for equality: give it back to if_version
    expires_at: datetime | None  # None for a key that never expires
```

A key's value with its version and expiry.

## Page

```python
@dataclass(frozen=True, slots=True)
class Page[T]:
    entries: list[Entry[T]]
    next: str | None
```

A page of a branch's keys, and where the next starts when there is one.

## Bucket

```python
class Bucket[T]:
    ...
```

A bucket's values under one branch of owners, the bucket's own when there are none.

Buckets come from ``store.bucket``, ``db.bucket`` and their ``under``;
inside a transaction, from ``tx.with_(bucket)``, whose calls go through
the transaction.

### Bucket.parts

```python
@property
def parts() -> Parts: ...
```

### Bucket.under

```python
def under(*owners: Key) -> Bucket[T]: ...
```

The branch the owners name below this one: whose keys these are.

### Bucket.get

```python
async def get(key: Key) -> T | None: ...
```

The key's value, None when it holds none.

### Bucket.entry

```python
async def entry(key: Key) -> Entry[T] | None: ...
```

The key's value with its version and expiry, None when it holds none.

### Bucket.has

```python
async def has(key: Key) -> bool: ...
```

### Bucket.set

```python
async def set(
    key: Key,
    value: T,
    *,
    ttl: Duration | None = None,
    expires_at: datetime | None = None,
    **check: Unpack[IfVersion],
) -> None: ...
```

Writes the value; a key that exists keeps its expiry unless the call gives one.

### Bucket.create

```python
async def create(
    key: Key,
    value: T,
    *,
    ttl: Duration | None = None,
    expires_at: datetime | None = None,
) -> bool: ...
```

Writes the value only where no live key is, and says whether it did.

### Bucket.add

```python
async def add(
    key: Key,
    *,
    ttl: Duration | None = None,
    expires_at: datetime | None = None,
) -> bool: ...
```

Puts the key in a bucket used as a set; False when it was there.

### Bucket.take

```python
async def take(key: Key, **check: Unpack[IfVersion]) -> T | None: ...
```

Reads the value and removes the key in one commit: of two callers taking a code at once, one gets it.

### Bucket.delete

```python
async def delete(key: Key, **check: Unpack[IfVersion]) -> bool: ...
```

Removes the key, and says whether it was there.

### Bucket.expire

```python
async def expire(
    key: Key,
    when: Duration | datetime,
    **check: Unpack[IfVersion],
) -> bool: ...
```

Gives a live key a new expiry, a span from now or a time, keeping its value; False when it holds none.

### Bucket.clear

```python
async def clear() -> None: ...
```

Removes this branch's keys and every branch under it, at once however many.

### Bucket.list

```python
async def list(*, limit: int | None = None, after: Key | None = None) -> Page[T]: ...
```

One page of this branch's own keys, in the byte order of their text: "10" before "9".

### Bucket.all

```python
async def all(*, limit: int | None = None) -> AsyncIterator[Entry[T]]: ...
```

Walks this branch's own keys a page at a time, holding nothing between pages.

A key written during the walk may or may not be met. It renews no idle key.

## Counters

```python
class Counters:
    ...
```

Numbers by key that only add up: 0 for one never added to, or expired. They come from ``store.counters``.

### Counters.parts

```python
@property
def parts() -> Parts: ...
```

### Counters.under

```python
def under(*owners: Key) -> Counters: ...
```

### Counters.add

```python
async def add(key: Key, n: int = 1) -> int: ...
```

Adds n, 1 unless given, and gives the new count.

### Counters.get

```python
async def get(key: Key) -> int: ...
```

### Counters.delete

```python
async def delete(key: Key) -> bool: ...
```

Removes the counter, and says whether it was there.

### Counters.clear

```python
async def clear() -> None: ...
```

Removes this branch's counters and every branch's under it.

## WindowUse

```python
@dataclass(frozen=True, slots=True)
class WindowUse:
    used: int
    limit: int
    left: int
    resets_at: datetime | None  # when the window resets; None for one not started
```

One window of a quota as a key stands in it.

## Allowance

```python
@dataclass(frozen=True, slots=True)
class Allowance:
    ok: bool
    left: int  # how many more would pass now
    retry_at: datetime | None  # when a refused request may try again; None when it passed
    windows: dict[str, WindowUse] = field(default_factory=dict[str, WindowUse])  # a quota's windows by name; none for a rate limit
```

What a rate limit or a quota answers a request.

## RateLimit

```python
class RateLimit:
    ...
```

A rate of requests a key: open it with ``store.rate_limit(name, rate=...)``.

### RateLimit.under

```python
def under(*owners: Key) -> RateLimit: ...
```

The rate limit of a branch, each key apart from the same key elsewhere.

### RateLimit.allow

```python
async def allow(key: Key, n: int = 1) -> Allowance: ...
```

Asks for n requests of key, one unless given: all pass or none; more than the burst is InvalidError.

### RateLimit.peek

```python
async def peek(key: Key) -> Allowance: ...
```

The answer one request would get, using nothing.

### RateLimit.reset

```python
async def reset(key: Key) -> None: ...
```

Forgets key, so that its next request starts afresh.

## Quota

```python
class Quota:
    ...
```

Uses by key in named windows: open it with ``store.quota(name, daily="100/1d")``.

### Quota.under

```python
def under(*owners: Key) -> Quota: ...
```

### Quota.allow

```python
async def allow(key: Key, n: int = 1) -> Allowance: ...
```

Uses n of key's every window, one unless given, or none when one has no room.

More than a window's limit is InvalidError.

### Quota.peek

```python
async def peek(key: Key) -> Allowance: ...
```

### Quota.reset

```python
async def reset(key: Key) -> None: ...
```

### Quota.refund

```python
async def refund(key: Key, n: int = 1) -> None: ...
```

Gives n uses back to every window of key, one unless given.

## Once

```python
class Once[T]:
    ...
```

Answers kept once a key: open them with ``store.once(name, type)``.

### Once.under

```python
def under(*owners: Key) -> Once[T]: ...
```

The answers of a branch, each key apart from the same key elsewhere.

### Once.run

```python
async def run(key: Key, fn: Callable[[], Awaitable[T] | T]) -> T: ...
```

The answer kept for key, or fn's, which is kept.

A run of a key that another runs, here or in another process, waits for
it and gets its answer; an exception of fn keeps nothing and is raised,
so the next run runs fn again. A connection lost after fn returned and
before its answer was kept is OutcomeUnknownError.

### Once.get

```python
async def get(key: Key) -> T | None: ...
```

The answer kept for key, None when none is.

### Once.delete

```python
async def delete(key: Key) -> bool: ...
```

Forgets the answer kept for key, so that its next run runs fn again.

## Tx

```python
class Tx:
    ...
```

The calls of a transaction, each through ``tx.with_(handle)``.

### Tx.with_

```python
@overload
def with_[T](handle: Bucket[T]) -> TxBucket[T]: ...
@overload
def with_(handle: Counters) -> TxCounters: ...
@overload
def with_[T](handle: Queue[T]) -> StoreTxQueue[T]: ...
@overload
def with_[T](handle: Channel[T]) -> TxChannel[T]: ...
def with_(
    handle: Bucket[Any] | Counters | Queue[Any] | Channel[Any],
) -> TxBucket[Any] | TxCounters | StoreTxQueue[Any] | TxChannel[Any]: ...
```

Takes a handle into the transaction: its calls then go through it.

### Tx.takes

```python
def takes(parts: Parts, file: Literal['kv.db', 'jobs.db'], what: str) -> None: ...
```

### Tx.read

```python
async def read(parts: Parts, key: Key) -> dict[str, Any]: ...
```

What the key holds as this transaction sees it: its own writes over what the store held.

### Tx.write

```python
def write(
    method: str,
    parts: Parts,
    fields: dict[str, Any],
    found: dict[str, Any] | None = None,
) -> None: ...
```

Keeps a write for the commit; found is what a read of its key answers after it.

### Tx.clear

```python
def clear(parts: Parts) -> None: ...
```

### Tx.after

```python
def after(fn: Callable[[], Awaitable[None]]) -> None: ...
```

What the commit does once it answered: a message published inside is handed over then.

### Tx.commit

```python
async def commit() -> None: ...
```

Commits the writes, then does what waited for the commit.

## TxBucket

```python
class TxBucket[T]:
    ...
```

A bucket's calls inside a transaction: reads answer at once, writes wait for its commit.

### TxBucket.under

```python
def under(*owners: Key) -> TxBucket[T]: ...
```

### TxBucket.get

```python
async def get(key: Key) -> T | None: ...
```

### TxBucket.entry

```python
async def entry(key: Key) -> Entry[T] | None: ...
```

### TxBucket.has

```python
async def has(key: Key) -> bool: ...
```

### TxBucket.set

```python
async def set(
    key: Key,
    value: T,
    *,
    ttl: Duration | None = None,
    expires_at: datetime | None = None,
    **check: Unpack[IfVersion],
) -> None: ...
```

### TxBucket.create

```python
async def create(
    key: Key,
    value: T,
    *,
    ttl: Duration | None = None,
    expires_at: datetime | None = None,
) -> bool: ...
```

Writes the value only where no live key is, and says whether it will.

### TxBucket.add

```python
async def add(
    key: Key,
    *,
    ttl: Duration | None = None,
    expires_at: datetime | None = None,
) -> bool: ...
```

### TxBucket.take

```python
async def take(key: Key, **check: Unpack[IfVersion]) -> T | None: ...
```

### TxBucket.delete

```python
async def delete(key: Key, **check: Unpack[IfVersion]) -> bool: ...
```

### TxBucket.expire

```python
async def expire(
    key: Key,
    when: Duration | datetime,
    **check: Unpack[IfVersion],
) -> bool: ...
```

### TxBucket.clear

```python
async def clear() -> None: ...
```

Removes this branch's keys and every branch's under it, with the commit.

## TxCounters

```python
class TxCounters:
    ...
```

Counters inside a transaction: an add commits with its writes.

### TxCounters.under

```python
def under(*owners: Key) -> TxCounters: ...
```

### TxCounters.add

```python
async def add(key: Key, n: int = 1) -> None: ...
```

Adds n, 1 unless given, with the commit; the count is known once it has.

<!-- Generated by task reference from sdk/python/src/tinystore/kv.py, sdk/python/src/tinystore/limits.py, sdk/python/src/tinystore/once.py, sdk/python/src/tinystore/tx.py. Edit the doc comments there, not this file. -->
