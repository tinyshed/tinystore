# Records API for Python

Every public class, function and type of the Records engine in `tinystore`, generated from its source. The [Records guide](../../records/README.md) explains how to use them, and the [Bun and Node](../bun/records.md) and [Go](../go/records.md) pages list the same API.

## Record

```python
@dataclass(frozen=True, slots=True)
class Record:
    at: int  # unix nanoseconds
    stream: str
    name: str
    level: int | None
    body: str | bytes | None
    trace_id: bytes | None
    span_id: bytes | None
    context: list[tuple[str, str | bytes]]
    attrs: list[tuple[str, str | bytes]]
```

A record as it was kept; a field's value is the JSON it was written as, spelled as given.

## fields

```python
def fields(pairs: Iterable[tuple[str, str | bytes]]) -> dict[str, Any]: ...
```

A record's fields, its attrs or its context, as a dict, each value as json.loads reads it.

The last of a repeated key is kept, and bytes that are not UTF-8 read
with U+FFFD in their place. The pairs keep what json.loads may not:
1.2300 as written.

    async for record in store.records.all(since="1h", streams=["readers"]):
        page = tinystore.fields(record.attrs)["page"]

## trace

```python
@contextmanager
def trace(
    trace_id: bytes | str,
    span_id: bytes | str | None = None,
) -> Generator[None]: ...
```

Runs a block in a trace, as Go's records.WithTrace carries one in a context.

A handler's lines, and records appended without a trace_id of their own,
inside the block and in the tasks it starts, take its trace and span:

    with tinystore.trace(trace_id, span_id):
        log.info("charged")  # a record of that trace and span

## context

```python
@contextmanager
def context(**fields: object) -> Generator[None]: ...
```

Runs a block whose log lines carry fields in their context, as Bun's log.with and Go's Logger.With do.

A handler's lines inside the block, and in the tasks it starts, take the
fields after their logger's name; an inner block adds its own, a field
named again taking its newer value:

    with tinystore.context(request_id=request_id):
        log.info("charged", extra={"amount": 10})  # request_id in the line's context

## Cursor

```python
@dataclass(frozen=True, slots=True)
class Cursor:
    segment: int = 0
    row: int = 0
```

Where a follower stands in the sealed segments, kept by the caller between follows.

## Records

```python
class Records:
    ...
```

### Records.append

```python
async def append(*records: Mapping[str, Any]) -> None: ...
```

Appends records in one transaction, all or none; a refused one names itself as call.

A record is a mapping of at (a datetime or unix nanoseconds, now when
absent), stream, name, level, body, trace_id, span_id, context, attrs.
A record of no trace_id takes the trace the call runs in, from trace().

### Records.scan

```python
async def scan(
    *,
    since: Duration | None = None,
    from_: datetime | int | None = None,
    to: datetime | int | None = None,
    streams: Iterable[str] | None = None,
    names: Iterable[str] | None = None,
    min_level: str | int | None = None,
    trace_id: bytes | str | None = None,
    attrs: Fields | None = None,
    context: Fields | None = None,
    search: str | None = None,
    newest: bool = False,
    limit: int | None = None,
    after: str | None = None,
) -> Page[Record, str]: ...
```

One page of the records a query matches, from one snapshot, in event-time order.

Over the last ``since``, or from ``from_`` to ``to``, in datetimes or
unix nanoseconds. The page's next, while a limit or a budget ended it
early, is passed back as ``after`` with the same query:

    page = await store.records.scan(since="1h", min_level="warn")
    more = await store.records.scan(since="1h", min_level="warn", after=page.next)

### Records.all

```python
async def all(
    *,
    since: Duration | None = None,
    from_: datetime | int | None = None,
    to: datetime | int | None = None,
    streams: Iterable[str] | None = None,
    names: Iterable[str] | None = None,
    min_level: str | int | None = None,
    trace_id: bytes | str | None = None,
    attrs: Fields | None = None,
    context: Fields | None = None,
    search: str | None = None,
    newest: bool = False,
    limit: int | None = None,
) -> AsyncIterator[Record]: ...
```

Every record a query matches, a page at a time.

### Records.follow

```python
async def follow(
    cursor: Cursor | None = None,
    limit: int | None = None,
) -> tuple[list[Record], Cursor, int]: ...
```

The sealed records after a cursor, the cursor to follow from next, and the segments expired first.

### Records.lines

```python
def lines(stream: str, *, buffer: int = 1 << 20) -> Lines: ...
```

A writer of another program's output that never waits, dropping and counting what does not fit.

### Records.handler

```python
def handler(
    stream: str,
    level: int = logging.NOTSET,
    *,
    buffer: int = 1024,
    console: Console | None = None,
    time: ConsoleTime | None = None,
    hide_stream: bool = False,
    to: TextIO | None = None,
    redact: Iterable[str] = (),
    keep_url_passwords: bool = False,
    replace: Callable[[str, Any], Any] | None = None,
    env: FromEnv | bool = True,
    source: bool = False,
) -> Handler: ...
```

A logging.Handler whose lines reach the console as they are logged and the stream once a second.

It never makes the logger wait. console is pretty on a terminal and
JSON otherwise when None, "off" for none. redact hides, in the store
and on the console, the fields whose keys name a secret, "api key"
hiding api_key and apiKey, and tinystore.SECRETS the usual ones; a
URL's password is hidden unless keep_url_passwords. LOG_LEVEL,
LOG_FORMAT and LOG_TIME win over the arguments, or those after env's
prefix, and env=False reads none; source adds where a line was logged.

### Records.damaged

```python
async def damaged() -> list[dict[str, Any]]: ...
```

The rows the server's records have met that no longer read.

### Records.drop

```python
async def drop(damage: Mapping[str, Any]) -> None: ...
```

Removes a damaged row, a repair an admin connection alone may make.

### Records.stop

```python
async def stop() -> None: ...
```

## SECRETS

```python
SECRETS = ('password', 'passwd', 'passphrase', 'secret', 'token', 'credential', 'credentials', 'authorization', 'cookie', 'api key', 'private key', 'secret key', 'access key', 'signing key', 'encryption key', 'connection string', 'dsn')
```

The names redact takes to hide the usual secrets, as Go's console.Secrets and Bun's secrets.

## ConsoleHandler

```python
class ConsoleHandler(logging.Handler):
    ...
```

A logging.Handler of the console alone: each record is written as it is logged, never kept.

It writes the lines store.records.handler writes, which keeps them too:
pretty on a terminal and one JSON object a line otherwise, on stderr
unless to says where. LOG_LEVEL, LOG_FORMAT and LOG_TIME win over the
arguments, or the variables of env's prefix, and env=False reads none. A
record's logger name is its context, its extra fields its attributes, an
exception its traceback under error. redact hides the fields whose keys
name a secret, SECRETS the usual ones, and a URL's password is hidden
unless keep_url_passwords; replace changes each value first; source adds
where the line was logged.

### ConsoleHandler.emit

```python
def emit(record: logging.LogRecord) -> None: ...
```

## handler

```python
def handler(
    stream: str,
    level: int = logging.NOTSET,
    *,
    console: Console | None = None,
    time: ConsoleTime | None = None,
    hide_stream: bool = False,
    to: TextIO | None = None,
    redact: Iterable[str] = (),
    keep_url_passwords: bool = False,
    replace: Callable[[str, Any], Any] | None = None,
    env: FromEnv | bool = True,
    source: bool = False,
) -> ConsoleHandler: ...
```

A logging.Handler of the console alone, for a program that wants the logger and not the records.

    logging.basicConfig(handlers=[tinystore.handler("app", redact=tinystore.SECRETS)], level=logging.INFO)

store.records.handler(stream) in its place keeps every line too.

<!-- Generated by task reference from sdk/python/src/tinystore/records.py, sdk/python/src/tinystore/_console.py. Edit the doc comments there, not this file. -->
