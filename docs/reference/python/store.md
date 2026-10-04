# Store API for Python

Every public class, function and type of the store in `tinystore`, generated from its source. The [Store guide](../../languages.md) explains how to use them, and the [Bun and Node](../bun/store.md) and [Go](../go/store.md) pages list the same API.

## Status

```python
@dataclass(frozen=True, slots=True)
class Status:
    server: str
    protocol: int
    engines: tuple[str, ...]
    capability: Literal['admin', 'data']
```

What a server is: its version, v0.1.0 or (devel), the protocol, its engines, the connection's capability.

## Store

```python
class Store:
    ...
```

The store: its engines, and one close. Opened by open or connect.

### Store.status

```python
async def status() -> Status: ...
```

What the server this store reaches is, as its WELCOME said it.

Its version, the protocol the connection speaks, the engines it serves
and what the connection may do. A client newer than its server learns
here what it may ask for; a call past it is UnimplementedError, naming
the server's version.

### Store.sql

```python
async def sql(name: str, *, migrations: Migrations | None = None) -> Database: ...
```

A database of the application's own, sql/&lt;name>.db.

The first open in the server, on an admin connection, applies its
migrations; every later one checks them against what the file applied.
Without migrations it opens the file as it is, an empty one if there is
none, and checks nothing.

### Store.backup

```python
async def backup(
    path: str | os.PathLike[str],
    *,
    files: Iterable[str] = (),
) -> None: ...
```

Writes a backup of the whole store to a zip at path while the store keeps working, as tinystore backup does.

The zip holds every engine's file with its size and checksum, which
tinystore restore checks. It is written beside path and renamed into
place once whole, so a backup that fails leaves no zip. It needs an
admin connection; a remote server sends the zip over it. It holds no
file but the engines' unless files names it: a file of the
application's inside the store's directory, such as files=["secret.key"].

### Store.close

```python
async def close() -> None: ...
```

Ingests the instruments' last values, then closes the connection.

## open

```python
def open(
    directory: str | os.PathLike[str],
    *,
    private: bool = False,
    binary: str | None = None,
    idle: Duration | None = None,
    clock: datetime | None = None,
) -> Opening: ...
```

Opens the store in a directory through its sidecar, found through SERVE or started.

private asks for a child of this process's own on stdin and stdout
instead, which lives and dies with it. idle is how long a sidecar this
process starts stays once its last connection has gone: 30 s unless
given, 0 for ever. clock runs a private server on a test's clock, from
that time, which store.clock moves forward instead of a test waiting:
keys expire, jobs come due and records age at once. It returns once the
server has answered.

## connect

```python
def connect(url: str, *, token: str, tls: ssl.SSLContext | None = None) -> Opening: ...
```

Connects to a remote server: tls:// checks its certificate before the token leaves.

## Clock

```python
class Clock:
    ...
```

The clock of a store opened with private=True and a clock.

Moving it forward expires keys, makes jobs due and ages records at once,
as Go's tinystore.Options.Clock does; a store on the system's time refuses
it with InvalidError:

    async with tinystore.open(tmp_path, private=True, clock=datetime(2026, 10, 3, 9, tzinfo=UTC)) as store:
        await store.clock.advance("1h")

### Clock.now

```python
async def now() -> datetime: ...
```

The time the server's clock reads.

### Clock.advance

```python
async def advance(by: Duration) -> datetime: ...
```

Moves the clock forward by a while, and gives the time it then reads.

### Clock.set

```python
async def set(to: datetime) -> datetime: ...
```

Sets the clock to a time, which may not be before its own, and gives it.

## TinystoreError

```python
class TinystoreError(Exception):
    code: ClassVar[str] = 'internal'
```

## InvalidError

```python
class InvalidError(TinystoreError):
    ...
```

The request cannot be done as asked.

## LimitError

```python
class LimitError(TinystoreError):
    ...
```

A bound: memory, size or count. Later, or smaller, it may go through.

A limit the store names says which, what the call would have taken of it,
and the bound: ``decoded samples``, 120000, 100000.

## ClosedError

```python
class ClosedError(TinystoreError):
    ...
```

The store, the handle or the connection closed.

## InUseError

```python
class InUseError(TinystoreError):
    ...
```

A name is taken.

## ConflictError

```python
class ConflictError(TinystoreError):
    ...
```

A condition or a version no longer holds: read again, then decide.

## CorruptError

```python
class CorruptError(TinystoreError):
    ...
```

Stored bytes no longer read.

## TooOldError

```python
class TooOldError(TinystoreError):
    ...
```

A time before its engine's window.

## TooNewError

```python
class TooNewError(TinystoreError):
    ...
```

A time past its engine's window, ahead of the store's clock.

## SuspendedError

```python
class SuspendedError(TinystoreError):
    ...
```

A metrics series in quarantine until its repair.

## OutcomeUnknownError

```python
class OutcomeUnknownError(TinystoreError):
    ...
```

A write whose commit may or may not have happened.

The connection was lost while it was in flight, or the server's commit
failed. Read what it wrote before writing again.

## PermissionDeniedError

```python
class PermissionDeniedError(TinystoreError):
    ...
```

The connection's capability does not allow it: a data token changing a schema.

## UnimplementedError

```python
class UnimplementedError(TinystoreError):
    ...
```

A method the server does not have.

## CallCancelledError

```python
class CallCancelledError(TinystoreError):
    ...
```

A call the client cancelled; named apart from asyncio's, which a cancelled task raises.

## UnavailableError

```python
class UnavailableError(TinystoreError):
    ...
```

The server is closing; the call was not run and may be sent on another connection.

## InternalError

```python
class InternalError(TinystoreError):
    ...
```

A fault of the server's own.

## ProtocolError

```python
class ProtocolError(TinystoreError):
    ...
```

A frame or a message that breaks the protocol; the connection ends with it.

## UnauthenticatedError

```python
class UnauthenticatedError(TinystoreError):
    ...
```

A remote connection whose token the server does not know.

## limits

```python
STORE_MEMORY: Final = 'store memory'
STORE_MEMORY_NOW: Final = 'store memory, now'
MATCHED_SERIES: Final = 'matched series'
DECODED_BLOCKS: Final = 'decoded blocks'
FETCHED_BYTES: Final = 'fetched bytes'
DECODED_SAMPLES: Final = 'decoded samples'
OUTPUT_SAMPLES: Final = 'output samples'
OUTPUT_BUCKETS: Final = 'output buckets'
RECORD_BYTES: Final = "bytes of a record, a block's"
APPEND_BYTES: Final = "bytes of records in one Append, a segment's; split it"
JOB_VALUE_BYTES: Final = "bytes of a job's value"
STEP_BYTES: Final = "bytes of a step's answer"
OBJECT_BYTES: Final = "bytes of an object, the bucket's MaxSize"
```

The names a LimitError's limit holds, as Go's constants and Bun's limits spell them.

Map a limit without matching its text:

    except tinystore.LimitError as err:
        if err.limit == tinystore.limits.DECODED_SAMPLES:
            ...

## Page

```python
class Page[T, After](NamedTuple):
    items: list[T]
    next: After | None
```

One page of a scan: its items, and where the next begins, None after the last.

The next page is the same scan with ``after=page.next``; a page still
unpacks as a pair: ``items, after = await bucket.scan()``.

<!-- Generated by task reference from sdk/python/src/tinystore/store.py, sdk/python/src/tinystore/clock.py, sdk/python/src/tinystore/errors.py, sdk/python/src/tinystore/limits.py, sdk/python/src/tinystore/_page.py. Edit the doc comments there, not this file. -->
