# Store API for Python

Every public class, function and type of the store in `tinystore`, generated from its source. The [Store guide](../../handbook/start/directory.md) explains how to use them, and the [Bun and Node](../bun/store.md), [Go](../go/store.md) and [Rust](../rust/store.md) pages list the same API.

## SelfMetrics

```python
class SelfMetrics(TypedDict, total=False):
    system: bool  # the machine it runs on: its process's memory and processor, the system's; True unless said
```

What a store opened with ``self_metrics`` measures beside its own.

## Status

```python
@dataclass(frozen=True, slots=True)
class Status:
    server: str  # its version: v0.1.0, or (devel) for one built from a checkout
    protocol: int
    engines: list[str]  # the engines it serves
    capability: Literal['admin', 'data', 'read']  # admin may change a schema and drop records; data reads and writes; read only reads
```

What a server is, as its WELCOME said it to this store's connection.

## Store

```python
class Store:
    metrics: Metrics  # Series by name and labels, their reads and aggregates, and the program's instruments.
    clock: Clock  # The test's clock of a private store opened with ``clock``; on any other, its calls are InvalidError.
```

The store in a directory, as this process reaches it. Close it once: ``async with`` does.

### Store.bucket

```python
@overload
def bucket[T](
    name: str,
    of: type[T],
    *,
    ttl: Duration | None = None,
    idle: Duration | None = None,
    encrypted: bool = False,
) -> Bucket[T]: ...
@overload
def bucket(
    name: str,
    of: None = None,
    *,
    ttl: Duration | None = None,
    idle: Duration | None = None,
    encrypted: bool = False,
) -> Bucket[Any]: ...
def bucket(
    name: str,
    of: Any = None,
    *,
    ttl: Duration | None = None,
    idle: Duration | None = None,
    encrypted: bool = False,
) -> Bucket[Any]: ...
```

A bucket of values of one type by key; without a type, any JSON value, or keys alone as a set:

sessions = store.bucket("sessions", Session, idle="30d")
codes = store.bucket("login-codes", int, ttl="15m")
seen = store.bucket("stripe-events", ttl="7d")

A str, bytes, int, float or bool is kept as itself; a dataclass, a
TypedDict, a pydantic model or a list or dict of them as JSON.
``encrypted=True`` keeps every value sealed with the store's
encryption key.

### Store.counters

```python
def counters(
    name: str,
    *,
    ttl: Duration | None = None,
    flush_every: Duration | None = None,
) -> Counters: ...
```

Numbers by key that only add up: ``store.counters("login-attempts", ttl="15m")``.

``flush_every`` keeps counts in memory and writes them that often, so
that an add costs no commit and a crash loses at most that span.

### Store.rate_limit

```python
def rate_limit(name: str, *, rate: str, burst: int | None = None) -> RateLimit: ...
```

A smooth rate of requests a key: ``store.rate_limit("api", rate="100/s", burst=20)``.

### Store.quota

```python
def quota(name: str, **windows: str) -> Quota: ...
```

Uses by key in named windows, each counted from a key's first use, all or none.

``store.quota("ai", daily="100/1d", weekly="300/7d")``

### Store.once

```python
@overload
def once[T](name: str, of: type[T], *, keep: Duration | None = None) -> Once[T]: ...
@overload
def once(name: str, of: None = None, *, keep: Duration | None = None) -> Once[Any]: ...
def once(name: str, of: Any = None, *, keep: Duration | None = None) -> Once[Any]: ...
```

A function run once a key, its JSON answer kept a day unless keep says.

``store.once("charges", Receipt)``

### Store.queue

```python
@overload
def queue[T](
    name: str,
    of: type[T],
    *,
    attempts: int | None = None,
    backoff: Backoff | None = None,
    timeout: Duration | None = None,
    concurrency: int | Concurrency | None = None,
    rate: str | None = None,
    dedupe: Duration | None = None,
    keep: Duration | None = None,
    max_waiting: int | None = None,
) -> Queue[T]: ...
@overload
def queue(
    name: str,
    of: None = None,
    *,
    attempts: int | None = None,
    backoff: Backoff | None = None,
    timeout: Duration | None = None,
    concurrency: int | Concurrency | None = None,
    rate: str | None = None,
    dedupe: Duration | None = None,
    keep: Duration | None = None,
    max_waiting: int | None = None,
) -> Queue[Any]: ...
def queue(
    name: str,
    of: Any = None,
    *,
    attempts: int | None = None,
    backoff: Backoff | None = None,
    timeout: Duration | None = None,
    concurrency: int | Concurrency | None = None,
    rate: str | None = None,
    dedupe: Duration | None = None,
    keep: Duration | None = None,
    max_waiting: int | None = None,
) -> Queue[Any]: ...
```

A queue of jobs of one type, JSON, run in the order of their time:

emails = store.queue("emails", Email, attempts=20, backoff=Backoff(initial="5s", max="30m"))
refreshes = store.queue("refreshes", Refresh, concurrency=Concurrency(total=8, group=2))

### Store.schedule

```python
def schedule(
    name: str,
    handler: Callable[[Run], Awaitable[Outcome | None]],
    *,
    every: Duration | None = None,
    cron: str | None = None,
    time_zone: str | None = None,
    attempts: int | None = None,
    backoff: Backoff | None = None,
    timeout: Duration | None = None,
) -> Schedule: ...
```

A repeat the code owns: one repeating job under name, and handler run at each of its times.

It starts at once, as a queue's work does. The code's repeat replaces
the one kept each time the program opens it:

    cleanup = store.schedule("cleanup", cleanup_deleted, cron="10 3 * * *", time_zone="Europe/Berlin")

### Store.channel

```python
@overload
def channel[T](name: str, of: type[T]) -> Channel[T]: ...
@overload
def channel(name: str, of: None = None) -> Channel[Any]: ...
def channel(name: str, of: Any = None) -> Channel[Any]: ...
```

A channel of messages of one type, JSON, which every subscriber listening at that moment receives:

messages = store.channel("messages", Chat)
await messages.under(room_id).publish(Chat(sender="ann", text="hi"))
sub = await messages.under(room_id).subscribe(lambda m: show(m.value))

### Store.lock

```python
def lock(
    name: str,
    *,
    ttl: Duration = '10s',
    max_hold: Duration | None = None,
) -> Lock: ...
```

Keys held one holder at a time, across every program of the store:

    invoices = store.lock("invoices", ttl="10s")
    async with invoices.hold(invoice.id) as lease:
        await charge(invoice, fence=lease.fence)

- ``ttl``: how long a lease lasts from its last renewal, 1 s to 1 hour;
  this program renews it at a third of it while the block runs.
- ``max_hold``: how long a lease lasts from its grant, however it is
  renewed: past it the next in line holds the key.

### Store.semaphore

```python
def semaphore(
    name: str,
    *,
    limit: int,
    ttl: Duration = '10s',
    max_hold: Duration | None = None,
) -> Semaphore: ...
```

Keys held by up to limit holders at a time, 1 to 1000:

uploads = store.semaphore("uploads", limit=3, ttl="30s")
async with uploads.hold(user.id):
    await upload(file)

### Store.files

```python
def files(
    name: str,
    *,
    ttl: Duration | None = None,
    max_file_size: Size | None = None,
) -> Files: ...
```

A named set of files by path, written whole and replaced whole:

avatars = store.files("avatars")
reports = store.files("reports", ttl="24h", max_file_size="2GiB")

### Store.events

```python
@overload
def events() -> AllEvents: ...
@overload
def events(stream: str, *, keep: Duration | None = None) -> Events: ...
def events(
    stream: str | None = None,
    *,
    keep: Duration | None = None,
) -> Events | AllEvents: ...
```

A stream of events by name, made with its first event; without a name, every stream, to read:

audit = store.events("audit", keep="365d")
await audit.append("role.changed", {"user": 42, "role": "admin"})
page = await store.events().list(since="1h", min_level="warn")

### Store.logger

```python
def logger(
    stream: str,
    *,
    level: Level | None = None,
    console: Console | None = None,
    time: ConsoleTime | None = None,
    hide_stream: bool = False,
    to: TextIO | None = None,
    redact: Iterable[str] = (),
    keep_url_passwords: bool = False,
    replace: Callable[[str, Any], Any] | None = None,
    source: bool = False,
    env: str | bool = True,
    buffer: int = 1024,
    keep: Duration | None = None,
) -> Logger: ...
```

A logger of the stream: each line printed as it is logged and kept within a second.

A call never waits; what its buffer has no room for is dropped and
counted. The store's close hands over what it holds:

    log = store.logger("api", redact=tinystore.secrets)
    log.info("signed in", user=42)

### Store.log_handler

```python
def log_handler(
    stream: str,
    level: int = logging.NOTSET,
    **options: Any,
) -> LogHandler: ...
```

A handler for a program that logs through logging, its lines printed and kept as the store's logger's:

logging.getLogger("worker").addHandler(store.log_handler("worker"))

A record's extra are its fields, its logger's name in its context.
The options are the store's logger's.

### Store.database

```python
async def database(
    name: str,
    *,
    migrations: Migrations | None = None,
    durability: Durability | None = None,
) -> Database: ...
```

The application's database name, sql/&lt;name>.db, once its migrations are applied and checked.

A migration that fails, or one changed after it was applied, is told
here, at the start. Without migrations the file opens as it is, an
empty one when there is none:

    db = await store.database("app", migrations=Path(__file__).parent / "migrations")

### Store.index

```python
@overload
def index[T](
    name: str,
    of: type[T],
    *,
    fields: Fields | None = None,
    filters: Sequence[str] = (),
    typo_tolerance: bool | None = None,
    vectors: Vectors | None = None,
    hybrid: Hybrid | None = None,
    rebuild: Literal['automatic', 'manual'] | None = None,
) -> SearchIndex[T]: ...
@overload
def index(
    name: str,
    of: None = None,
    *,
    fields: Fields | None = None,
    filters: Sequence[str] = (),
    typo_tolerance: bool | None = None,
    vectors: Vectors | None = None,
    hybrid: Hybrid | None = None,
    rebuild: Literal['automatic', 'manual'] | None = None,
) -> SearchIndex[Any]: ...
def index(
    name: str,
    of: Any = None,
    *,
    fields: Fields | None = None,
    filters: Sequence[str] = (),
    typo_tolerance: bool | None = None,
    vectors: Vectors | None = None,
    hybrid: Hybrid | None = None,
    rebuild: Literal['automatic', 'manual'] | None = None,
) -> SearchIndex[Any]: ...
```

An index of documents of one type by id, found again by their words, vectors or both:

    articles = store.index("articles", Article, fields={"title": 2, "body": 1})
    result = await articles.search("как испечь хлеб")

Opening an index with other settings than it holds changes it, and
one that needs every document read again does so beside it, the old
rows answering until the new ones switch in; ``rebuild="manual"``
waits for ``store.reindex``.

### Store.reindex

```python
async def reindex(
    name: str,
    of: Any = None,
    *,
    fields: Fields | None = None,
    filters: Sequence[str] = (),
    typo_tolerance: bool | None = None,
    vectors: Vectors | None = None,
    hybrid: Hybrid | None = None,
    rebuild: Literal['automatic', 'manual'] | None = None,
) -> SearchIndex[Any]: ...
```

Opens an index as index does and reads it again beside itself in the background, as a change would.

### Store.drop_index

```python
async def drop_index(name: str) -> bool: ...
```

Drops an index and every document it holds, in the background; says whether there was one.

### Store.indexes

```python
async def indexes() -> list[IndexInfo]: ...
```

Every index the store holds: its documents and vectors, when it was last opened, and a rebuild under way.

### Store.tx

```python
async def tx[R](fn: Callable[[Tx], Awaitable[R]]) -> R: ...
```

Runs fn as one transaction over the handles it takes in with ``tx.with_(handle)``, and gives what it returns.

Reads go at once; writes commit together when fn returns, after a check
that nothing it read has changed, and fn runs again when something did,
five times at most, so it does nothing else that must happen once.

### Store.config

```python
@overload
async def config[T](
    name: str,
    layers: Sequence[type[T] | Layer],
    *,
    validate: Callable[[T], object] | type | None = None,
    strict: bool = False,
) -> Config[T]: ...
@overload
async def config(
    name: str,
    layers: Sequence[Any],
    *,
    validate: Callable[[Any], object] | type | None = None,
    strict: bool = False,
) -> Config[Any]: ...
async def config(
    name: str,
    layers: Sequence[Any],
    *,
    validate: Callable[[Any], object] | type | None = None,
    strict: bool = False,
) -> Config[Any]: ...
```

A program's settings as one typed value: the declaration first, then dicts, files and the environment.

Each layer goes over the ones before it, and a live setting's change
over them all:

    config = await store.config("app", [Settings, file("app.yaml"), env("APP", vars={"port": "PORT"})])

It follows every change, whoever makes it, until the store closes; the
same config opened again in the process shares its one stream.
``validate``, a function that raises or a pydantic model, checks the
whole value at open and before a change is kept; ``strict`` refuses to
open while a kept change no longer fits, rather than ignoring it.

### Store.backup

```python
async def backup(
    path: str | os.PathLike[str],
    *,
    include: Sequence[str] = (),
    include_encryption_key: bool = False,
) -> Backup: ...
```

A backup of every engine of the store as one zip, made while the store keeps working.

It is written beside path and renamed once whole, so that a backup
that fails leaves no zip. A file of the program's goes in only when
``include`` names it by its path in the store's directory, and the
store's encryption key only with ``include_encryption_key``, the zip
not being encrypted:

    backup = await store.backup("backup-2026-10-13.zip", include=["secret.key"])
    if backup.key_left_out:
        print("keep encryption.key apart: the backup opens with it alone")

### Store.status

```python
async def status() -> Status: ...
```

What the server this store reaches is: its version, protocol, engines and capability.

A client newer than its server learns here what it may ask for; a call
past it is UnimplementedError, naming the server's version.

### Store.close

```python
async def close() -> None: ...
```

Stops every worker, each once its handlers under way have answered, then closes the connection.

Before the connection closes it cancels every subscription, ``each``
loop, tail and watch, whose loops end with ClosedError. A private
child or the core in this process lets the directory go before this
returns; the directory's sidecar goes once it has been idle.

## open

```python
async def open(
    directory: str | os.PathLike[str],
    *,
    private: bool = False,
    embedded: bool = False,
    library: str | None = None,
    binary: str | None = None,
    idle: Duration | None = None,
    clock: datetime | None = None,
    durability: Durability | None = None,
    encryption_key_file: str | os.PathLike[str] | None = None,
    self_metrics: bool | SelfMetrics = False,
    max_series: int | None = None,
) -> Store: ...
```

Opens the store in a directory, and returns once it answers.

Through the directory's sidecar, found through its SERVE file or started,
unless asked otherwise: ``private=True`` starts a server of this process's
own, a child that lives and dies with it, for tests and scripts;
``embedded=True`` runs the core inside this process, through the module
this platform's wheel carries or the ``library`` given. A directory that
cannot be served fails here. A cancel or a timeout around it, such as
``asyncio.timeout``, stops it at once and keeps no connection; the
``CancelledError`` carries a note of what it waited for.

- ``binary``: the tinystore binary; TINYSTORE_BIN, this platform's wheel's
  or PATH's when absent.
- ``idle``: how long a sidecar this process starts stays once its last
  connection has gone; 30 s unless given, ``"0s"`` for ever. One found
  running keeps its own.
- ``clock``: runs a private server on a test's clock, starting at this
  time, which ``store.clock`` moves forward instead of a test waiting.
- ``durability``: how far a commit of the store's files goes before it
  returns. Unless given, kv, jobs, files, search and every database commit
  ``"full"``, and records and metrics ``"os"``; given, every file commits
  so. ``"os"`` does not wait for the disk. It is the store's, set by
  whoever opens it first: one found open with another, or with none when
  this asks one, is refused.
- ``encryption_key_file``: the file of the store's encryption key, 64 hex
  digits, kept apart from the data; the store reads it and never makes it.
- ``self_metrics``: measures the store into its own metrics, under names
  that start with ``tinystore_``: its commits and their waits, its files,
  its memory and the machine it runs on, which ``{"system": False}``
  leaves out. Off unless asked for; a sidecar found running keeps its own.
- ``max_series``: the metrics series the store holds at most, a new
  series past them a LimitError; 100,000 unless given. A sidecar found
  running keeps its own.

## connect

```python
async def connect(
    url: str,
    *,
    token: str,
    tls: ssl.SSLContext | None = None,
) -> Store: ...
```

Connects to a remote server.

``tls://`` checks the server's certificate before the token leaves;
``tcp://`` sends the token in the clear, for a network its operator trusts.
A cancel or a timeout around it stops it as it stops ``open``.

## Clock

```python
class Clock:
    ...
```

The clock of a store opened with ``private=True, clock=...``.

Moving it forward expires keys, makes jobs due and ages records at once;
a store on the system's time refuses it with InvalidError:

    store = await tinystore.open(dir, private=True, clock=datetime(2026, 10, 9, 9, tzinfo=UTC))
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

## Constraint

```python
@dataclass(frozen=True, slots=True)
class Constraint:
    kind: Literal['unique', 'primaryKey', 'foreignKey', 'check', 'notNull']
    table: str | None  # the table, when SQLite names it
    columns: list[str]  # the columns, when SQLite names them
    name: str | None  # a check's name, or its text when it has none; the index of a unique key on an expression
```

A constraint a database's write broke, as SQLite names it.

A unique or a primary key already held is a ConflictError, any other an
InvalidError:

    except tinystore.ConflictError as err:
        if err.constraint and "email" in err.constraint.columns:
            return "that email is taken"

## TinystoreError

```python
class TinystoreError(Exception):
    code: ClassVar[str] = 'internal'
    message: str
```

## InvalidError

```python
class InvalidError(TinystoreError):
    ...
```

The request cannot be done as asked.

## NotFoundError

```python
class NotFoundError(TinystoreError):
    ...
```

What the call needs is not there: a file to copy or rename.

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

A name is taken, or another process holds what was asked for.

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

A metrics series whose bytes no longer read, set aside until it is deleted.

## OutcomeUnknownError

```python
class OutcomeUnknownError(TinystoreError):
    ...
```

A write whose commit may or may not have happened.

The connection was lost while it was in flight, or the store's commit
failed. Read what it wrote before writing again.

## CommittedLocallyError

```python
class CommittedLocallyError(TinystoreError):
    ...
```

A synchronous commit its home made that no copy it waits for held in time.

The write stands, and losing that home's disk would lose it.

## LostError

```python
class LostError(TinystoreError):
    ...
```

A lease that ran out or was revoked at its max_hold while its function ran.

Another holder may have run beside it. The function's work stands, and a
system that compares fences refused what came late.

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

A call the store cancelled; named apart from asyncio's, which a cancelled task raises.

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

A fault of the store's own.

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

<!-- Generated by task reference from sdk/python/src/tinystore/store.py, sdk/python/src/tinystore/clock.py, sdk/python/src/tinystore/errors.py. Edit the doc comments there, not this file. -->
