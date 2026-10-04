# KV API for Python

Every public class, function and type of the KV engine in `tinystore`, generated from its source. The [KV guide](../../kv/README.md) explains how to use them, and the [Bun and Node](../bun/kv.md) and [Go](../go/kv.md) pages list the same API.

## Entry

```python
@dataclass(frozen=True, slots=True)
class Entry[V]:
    value: V
    version: str  # compared only for equality: give it back as if_version
    expires: datetime | None
    key: str | bytes = ''  # a scan's entry alone
```

## Kv

```python
class Kv:
    ...
```

### Kv.config

```python
async def config[T](
    name: str,
    of: type[T],
    /,
    *layers: Mapping[str, Any] | FromEnv,
    validate: Callable[[T], object] | None = None,
) -> Config[T]: ...
```

The config name, of a type whose defaults are its own: a dataclass, or a model.

Each layer goes over the one before, a file's values or from_env, and what update kept over
them all. It returns once the server's state is read, and follows every change from then on,
whoever makes it, until the store closes:

    cfg = await store.kv.config("app", Settings, toml, from_env("APP", ".env"))
    cfg.value.port                     # APP_PORT=3000 in .env makes it 3000
    await cfg.update({"port": 4000})   # kept: 4000 after a restart too

### Kv.limiter

```python
def limiter(name: str, /, *, rate: str, burst: int | None = None) -> Limiter: ...
```

A limiter of requests by key: `store.kv.limiter("api", rate="100/s", burst=20)`.

### Kv.quota

```python
def quota(name: str, /, **windows: str) -> Quota: ...
```

A quota of uses by key, its windows by name, all of them counted together or none.

Each window starts at a key's first use after the last ended:

    ai = store.kv.quota("ai", session="100/5h", weekly="300/7d")

### Kv.once

```python
def once[V](
    name: str,
    of: type[V],
    /,
    *,
    default_ttl: Duration | None = None,
) -> Once[V]: ...
```

The answers a function gives once a key, of one type, kept a day unless default_ttl says.

### Kv.stop

```python
def stop() -> None: ...
```

Stops following configs, as the store does when it closes.

### Kv.bucket

```python
@overload
def bucket(
    name: str,
    /,
    *,
    default_ttl: Duration | None = None,
    sliding: Duration | None = None,
) -> Bucket[Any]: ...
@overload
def bucket[V](
    name: str,
    of: type[V],
    /,
    *,
    default_ttl: Duration | None = None,
    sliding: Duration | None = None,
) -> Bucket[V]: ...
@overload
def bucket(
    name: str,
    of: None,
    /,
    *,
    default_ttl: Duration | None = None,
    sliding: Duration | None = None,
) -> Bucket[None]: ...
def bucket(
    name: str,
    of: Any = _MISSING,
    /,
    *,
    default_ttl: Duration | None = None,
    sliding: Duration | None = None,
) -> Bucket[Any]: ...
```

A bucket of values of one type: a type of the table above, or any JSON can hold.

### Kv.counters

```python
def counters(
    name: str,
    /,
    *,
    default_ttl: Duration | None = None,
    lose_at_most: Duration | None = None,
) -> Counters: ...
```

Counters: an int64 a key, 0 when absent; lose_at_most keeps changes in memory between flushes.

### Kv.batch

```python
def batch() -> Batch: ...
```

Calls in one transaction, all or none, sent as the async with block ends:

async with store.kv.batch() as tx:
    sessions.with_tx(tx).set(token, session)
    codes.with_tx(tx).delete(code)

### Kv.view

```python
def view() -> Batch: ...
```

Gets and hases from one snapshot, their futures settled as the block ends.

### Kv.tx

```python
async def tx[T](fn: Callable[[Tx], Awaitable[T]]) -> T: ...
```

Runs fn as one transaction that reads before it decides what to write, as Go's Tx does.

No writer is held across the network: a read goes to the server at
once, a write waits, and when fn returns the writes commit in one batch
that first checks every key fn read is still as it read it. When another
write changed one meanwhile, fn runs again, five times at most before
ConflictError, so it does nothing else that must happen once. It gives
back what fn returns:

    async def sign_in(tx: tinystore.Tx) -> int:
        user_id = await codes.with_tx(tx).take(digest(code))
        if user_id is None:
            raise InvalidCode
        sessions.of(user_id).with_tx(tx).set(digest(token), Session(device))
        return user_id

    user_id = await store.kv.tx(sign_in)

## Batch

```python
class Batch:
    ...
```

The calls of a batch or a view: each returns a future that settles once the batch has.

### Batch.record

```python
def record(
    method: str,
    open_body: bytes,
    fields: dict[str, Any],
    decode: Callable[[dict[str, Any]], Any],
) -> asyncio.Future[Any]: ...
```

## Tx

```python
class Tx:
    ...
```

A transaction of store.kv.tx.

A read goes to the server at once and is remembered with its version, so
that a key read again answers the same; a write waits for the commit, and a
key read after it answers what it wrote.

### Tx.record

```python
def record(
    method: str,
    open_body: bytes,
    fields: dict[str, Any],
    decode: Callable[[dict[str, Any]], Any],
) -> asyncio.Future[Any]: ...
```

### Tx.commit

```python
async def commit() -> None: ...
```

Sends what fn wrote in one batch, after a check of each key it read.

A key it found must still be at the version it found, and a key it
found absent still absent. A tx that only read checks its reads from
one snapshot.

### Tx.stale

```python
def stale(err: ConflictError) -> bool: ...
```

Whether the commit failed for a key fn read that changed since, which another run reads anew.

## Bucket

```python
class Bucket[V]:
    ...
```

A bucket's values under one branch of owners: sessions.of(user.id).get(token).

### Bucket.of

```python
def of(*owners: Key) -> Bucket[V]: ...
```

The branch below this one that the owners name.

### Bucket.with_tx

```python
def with_tx(tx: Batch | Tx) -> BucketTx[V]: ...
```

This bucket's calls inside a batch or a view, or inside a tx, whose reads answer at once.

### Bucket.get

```python
async def get(key: Key) -> V | None: ...
```

The key's value, None when it holds none.

### Bucket.get_entry

```python
async def get_entry(key: Key) -> Entry[V] | None: ...
```

### Bucket.has

```python
async def has(key: Key) -> bool: ...
```

### Bucket.set

```python
async def set(
    key: Key,
    value: V,
    *,
    ttl: Duration | None = None,
    expire_at: datetime | None = None,
    if_version: str | None = None,
) -> None: ...
```

Writes the value; a live key keeps its expiry unless one is given.

### Bucket.set_entry

```python
async def set_entry(
    key: Key,
    value: V,
    *,
    ttl: Duration | None = None,
    expire_at: datetime | None = None,
    if_version: str | None = None,
) -> Entry[V]: ...
```

### Bucket.set_if_absent

```python
async def set_if_absent(
    key: Key,
    value: V,
    *,
    ttl: Duration | None = None,
    expire_at: datetime | None = None,
) -> bool: ...
```

Writes the value only where no live key is, and says whether it did.

### Bucket.set_entry_if_absent

```python
async def set_entry_if_absent(
    key: Key,
    value: V,
    *,
    ttl: Duration | None = None,
    expire_at: datetime | None = None,
) -> tuple[bool, Entry[V]]: ...
```

Writes only where no live key is: whether it did, and the new key's entry or the live one's.

### Bucket.delete

```python
async def delete(key: Key, *, if_version: str | None = None) -> None: ...
```

### Bucket.take

```python
async def take(key: Key, *, if_version: str | None = None) -> V | None: ...
```

Reads the value and deletes the key in one write: a one-time code read and burned.

### Bucket.touch

```python
async def touch(
    key: Key,
    *,
    ttl: Duration | None = None,
    expire_at: datetime | None = None,
    if_version: str | None = None,
) -> bool: ...
```

Gives a live key a new expiry, keeping its value and version; False when it holds none.

### Bucket.clear

```python
async def clear() -> None: ...
```

Removes this branch's keys and every branch under it, at once however many.

### Bucket.scan

```python
async def scan(
    *,
    after: Key | None = None,
    limit: int | None = None,
) -> Page[Entry[V], str | bytes]: ...
```

One page of this branch's own keys in the byte order of their text, and the key the next begins after.

### Bucket.all

```python
async def all(*, limit: int | None = None) -> AsyncIterator[Entry[V]]: ...
```

Walks this branch's own keys a page at a time, holding no snapshot between pages.

## Counters

```python
class Counters:
    ...
```

Counters under one branch: an int64 a key, 0 when absent; a sum past int64 is refused.

### Counters.of

```python
def of(*owners: Key) -> Counters: ...
```

### Counters.get

```python
async def get(key: Key) -> int: ...
```

### Counters.add

```python
async def add(key: Key, n: int = 1) -> int: ...
```

Adds n and gives the new value.

### Counters.max

```python
async def max(key: Key, n: int) -> int: ...
```

Keeps the larger of the counter and n, and gives it.

### Counters.delete

```python
async def delete(key: Key) -> None: ...
```

### Counters.clear

```python
async def clear() -> None: ...
```

## Once

```python
class Once[V]:
    ...
```

Answers kept once a key, as Go's kv.Once keeps them: open them with `store.kv.once(name, Receipt)`.

The server lets one call of a key run its function at a time, every client of the store included,
and keeps what it returned, so that a request sent again is answered as the first was.

### Once.of

```python
def of(*owners: Key) -> Once[V]: ...
```

The answers of a branch, each key apart from the same key elsewhere.

### Once.run

```python
async def run(key: Key, fn: Callable[[], Awaitable[V]]) -> V: ...
```

The answer kept under key, or fn's, which is kept.

A call of a key another call is running waits for it and gets its answer; an exception of
fn keeps nothing and is raised, so the next call runs fn again. A connection lost after fn
returned and before its answer was kept is OutcomeUnknownError:

    receipt = await charges.run(request_id, lambda: pay.charge(order, request_id))

### Once.get

```python
async def get(key: Key) -> V | None: ...
```

The answer kept under key, None when none is.

### Once.delete

```python
async def delete(key: Key) -> None: ...
```

Forgets the answer kept under key, so that the next run runs again.

## Allowance

```python
class Allowance(NamedTuple):
    ok: bool
    left: int  # how many more requests would pass now
    retry_after: float  # seconds until the request would pass; 0 when ok
```

What a limiter answers a request.

## Limiter

```python
class Limiter:
    ...
```

A limiter: open it with `store.kv.limiter(name, rate="100/s")`.

### Limiter.of

```python
def of(*owners: Key) -> Limiter: ...
```

The limiter of a branch, each key apart from the same key elsewhere.

### Limiter.allow

```python
async def allow(key: Key, n: int = 1) -> Allowance: ...
```

Asks for n requests of key: all pass or none does; past the burst is InvalidError.

## WindowUsage

```python
class WindowUsage(NamedTuple):
    used: int
    limit: int
    left: int
    reset_at: datetime | None  # when the window ends and the next use starts another; None before it starts
```

One window of a key: what it used of its limit, and when it resets.

## QuotaUsage

```python
class QuotaUsage(NamedTuple):
    ok: bool
    left: int  # how many more uses would pass now
    retry_after: float  # seconds until a refused use would pass; 0 when ok
    windows: dict[str, WindowUsage]
```

What a quota answers a key: what a limiter answers, and each window by its name.

## Quota

```python
class Quota:
    ...
```

A quota: open it with `store.kv.quota(name, session="100/5h", weekly="300/7d")`.

### Quota.of

```python
def of(*owners: Key) -> Quota: ...
```

The quota of a branch, each key apart from the same key elsewhere.

### Quota.allow

```python
async def allow(key: Key, n: int = 1) -> QuotaUsage: ...
```

Uses n of key's every window, or none when one has no room; past a window's limit is InvalidError.

### Quota.get

```python
async def get(key: Key) -> QuotaUsage: ...
```

Key's windows without using them: ok says whether one more use would pass now.

### Quota.refund

```python
async def refund(key: Key, n: int = 1) -> None: ...
```

Gives n uses back to each of key's windows that has not reset since.

### Quota.delete

```python
async def delete(key: Key) -> None: ...
```

Forgets key's windows, so that its next use starts each anew.

## secret

```python
def secret(variable: str | None = None, *, default: str | None = None) -> Any: ...
```

A secret field of a config's dataclass, never kept by update and shown as *** by sources().

It reads its own variable when given one, and is required without a default:

    url: str = secret("DATABASE_URL")

## fixed

```python
def fixed(
    default: Any = dataclasses.MISSING,
    *,
    variable: str | None = None,
) -> Any: ...
```

A field the defaults, a file or the environment set alone: update refuses it, sources() says where it came from.

It is required without a default:

    addr: str = fixed(":8080")

## FromEnv

```python
@dataclass(frozen=True, slots=True)
class FromEnv:
    prefix: str
    files: tuple[str | os.PathLike[str], ...]
```

The environment as a layer of a config, which from_env makes.

## from_env

```python
def from_env(prefix: str = '', *files: str | os.PathLike[str]) -> FromEnv: ...
```

The environment as a config's layer: from_env("APP") reads db.pool from APP_DB_POOL, from_env() from DB_POOL.

The .env files given are read first, the process's own variables over them, a missing file skipped.

## Source

```python
@dataclass(frozen=True, slots=True)
class Source:
    path: str
    value: str  # its JSON, or *** for a secret
    from_: str  # 'default', 'file', 'env NAME' or 'kept'
    ignored: str | None = None  # why a kept value is left out, when it is
```

Where a field's value came from.

## Config

```python
class Config[T]:
    ...
```

A config: open it with `await store.kv.config(name, Type, *layers)`, whose defaults are its own.

### Config.value

```python
@property
def value() -> T: ...
```

The config now, made anew after each change; update changes it.

### Config.update

```python
async def update(change: Mapping[str, Any]) -> None: ...
```

Changes the fields change names, at any depth: each one that changed is checked, kept, and seen at once.

A change validate refuses, one of a fixed field or a secret, or one leaving a required field empty, is
InvalidError and keeps nothing.

### Config.reset

```python
async def reset(*paths: str) -> None: ...
```

Forgets what update kept for paths, each a field or a group of them; all of it without paths.

### Config.watch

```python
def watch(fn: Callable[[T], object]) -> Callable[[], None]: ...
```

Calls fn with the config now and after each change, whoever made it; what it returns stops that.

### Config.sources

```python
def sources() -> list[Source]: ...
```

Where each field's value came from, and why a kept value is left out.

### Config.start

```python
async def start() -> None: ...
```

Follows the config's changes until the store closes; returns once the first state is read.

### Config.stop

```python
def stop() -> None: ...
```

Stops following the config, as the store does when it closes.

<!-- Generated by task reference from sdk/python/src/tinystore/kv.py, sdk/python/src/tinystore/limiter.py, sdk/python/src/tinystore/quota.py, sdk/python/src/tinystore/config.py. Edit the doc comments there, not this file. -->
