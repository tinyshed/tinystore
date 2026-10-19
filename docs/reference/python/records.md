# Records API for Python

Every public class, function and type of the Records engine in `tinystore`, generated from its source. The [Records guide](../../chat/what-it-is-doing.md) explains how to use them, and the [Bun and Node](../bun/records.md), [Go](../go/records.md) and [Rust](../rust/records.md) pages list the same API.

## Event

```python
@dataclass(frozen=True, slots=True)
class Event:
    name: str  # what happened; log for a logger's line
    fields: Mapping[str, Any] = field(default_factory=dict[str, Any])  # what happened, any object JSON writes
    at: datetime | None = None
    context: Mapping[str, Any] = field(default_factory=dict[str, Any])  # what it happened in: who asked, which request
    level: EventLevel | None = None
    message: str | None = None
    trace_id: str | None = None  # 32 hex digits: the current trace's unless given
    span_id: str | None = None  # 16 hex digits
    stream: str | None = None  # the stream a read found it in
```

An event: what an append writes, and what a read gives back.

A read fills every field; an append gives a name and what it knows, its
time now unless given, from what the stream keeps to ten minutes ahead.

## EventPage

```python
@dataclass(frozen=True, slots=True)
class EventPage:
    events: list[Event]
    next: str | None
```

A page of events, and the page after it; the last page has none.

## Top

```python
@dataclass(frozen=True, slots=True)
class Top:
    value: Any
    count: int
```

One of a key's most frequent values, and how many events hold it.

## Stats

```python
@dataclass(frozen=True, slots=True)
class Stats:
    samples: int
    sum: int | float
    min: int | float
    max: int | float
    avg: float
```

A field's numbers in a row: samples are the numbers, which avg divides sum by.

## Row

```python
@dataclass(frozen=True, slots=True)
class Row:
    at: datetime  # where the row's span begins, counted from 1970 in UTC
    count: int
    by: dict[str, Any]
    stats: dict[str, Stats]
    distinct: dict[str, int]
```

A row of an aggregate: a span of every and a value of each key of by, with its count.

## StreamInfo

```python
@dataclass(frozen=True, slots=True)
class StreamInfo:
    name: str
    keep: str
    retained_from: datetime  # its oldest event, or where its keep begins while it holds older
    newest: datetime
```

A stream with events: what it keeps, where that begins, and its newest event's time.

## PollBatch

```python
@dataclass(frozen=True, slots=True)
class PollBatch:
    events: list[Event]
    cursor: str  # where the next poll begins, kept once these events are safe
    expired: int  # the events keep removed before the cursor reached them
```

Every event once, a batch a call: its events, where the next poll begins, and what expired before it.

## TailBatch

```python
@dataclass(frozen=True, slots=True)
class TailBatch:
    events: list[Event]
    missed: int
```

A tail's batch: the events written since the last, and how many it missed.

## Damage

```python
@dataclass(frozen=True, slots=True)
class Damage:
    part: Literal['head row', 'segment']
    id: int
    stream: str
    first: datetime
    last: datetime
    error: str
```

A head row or a segment that no longer reads, as damaged lists it and drop takes it.

## Query

```python
class Query(Counted, total=False):
    newest: bool  # newest first; oldest first without it
    limit: int  # the events of a page: 1000, and 10,000 at most
    after: str  # a page's next, with the rest of the query as it was
```

What a list finds, and how its page reads.

## Events

```python
class Events(_Reads):
    stream: str
```

A stream of events: written with append, which waits for the file, and read back.

### Events.append

```python
async def append(
    name_or_events: str | Sequence[Event],
    fields: Mapping[str, Any] | None = None,
    /,
    *,
    at: datetime | None = None,
    context: Mapping[str, Any] | None = None,
    level: EventLevel | None = None,
    message: str | None = None,
    trace_id: str | None = None,
    span_id: str | None = None,
) -> None: ...
```

Writes an event, or a list of them whole or not at all.

It returns once they are written, and a read finds them at once. A
refused event names its place in the list and its name.

### Events.append_lines

```python
async def append_lines(lines: list[Line]) -> None: ...
```

A logger's lines, each an event of this stream.

### Events.lines

```python
def lines() -> Lines: ...
```

A writer of another program's output into this stream, each line an event at the time it arrived.

A stack trace's or a JSON value's lines make one, and a write never waits:

    async with store.events("worker").lines() as out:
        async for chunk in child.stdout:
            await out.write(chunk)

## AllEvents

```python
class AllEvents(_Reads):
    ...
```

Every stream's events, to read: ``store.events()``.

### AllEvents.streams

```python
async def streams() -> list[StreamInfo]: ...
```

Every stream with events, by name, with what it keeps, where that begins and its newest event's time.

### AllEvents.poll

```python
async def poll(cursor: str | None = None, *, limit: int | None = None) -> PollBatch: ...
```

Every event once, a batch a call, from a cursor the program keeps, the oldest event kept without one.

With no event past the cursor, the batch is empty and its cursor the
one given.

### AllEvents.damaged

```python
async def damaged() -> list[Damage]: ...
```

What reads met that no longer reads, which drop removes: each head row and segment with its times.

### AllEvents.drop

```python
async def drop(damage: Damage) -> None: ...
```

Removes a head row or a segment that damaged listed, so that the reads over it work again.

## Lines

```python
class Lines:
    ...
```

A writer of another program's output into a stream, as ``events.lines()`` begins it.

Each line becomes an event, and a write never waits. It sends whole
lines, so that what it has no room for, dropped counts as lines.

### Lines.dropped

```python
@property
def dropped() -> int: ...
```

The lines dropped: past what the writer holds, and, once it ended, by the store.

### Lines.write

```python
async def write(chunk: bytes | str) -> None: ...
```

Takes a piece of output, cut anywhere; it never waits.

### Lines.end

```python
async def end() -> None: ...
```

Ends the writer: what it holds goes, a last line without its newline too, once the store wrote them.

## secrets

```python
secrets = SECRETS
```

The names redact takes to hide the usual secrets: password, token, api key, connection string and the rest.

## Logger

```python
class Logger:
    ...
```

A logger of one stream, as ``store.logger`` and ``tinystore.logger`` make it.

``info``, ``warning`` and the rest return at once; a line goes to the
console as it is logged, and when the logger is a store's becomes an
event named log, its message the line's text, the call's fields its
fields and bind's its context.

### Logger.stream

```python
@property
def stream() -> str: ...
```

### Logger.dropped

```python
@property
def dropped() -> int: ...
```

The lines dropped since the logger began: a full buffer, or a write that failed.

### Logger.debug

```python
def debug(message: str, /, **fields: Any) -> None: ...
```

### Logger.info

```python
def info(message: str, /, **fields: Any) -> None: ...
```

### Logger.warning

```python
def warning(message: str, /, **fields: Any) -> None: ...
```

### Logger.error

```python
def error(message: str, /, **fields: Any) -> None: ...
```

### Logger.exception

```python
def exception(message: str, /, **fields: Any) -> None: ...
```

An error, the exception being handled kept as its traceback under error.

### Logger.event

```python
def event(name: str, /, **fields: Any) -> None: ...
```

An event of the stream: named for itself, with no level and no message, written as a line is.

### Logger.bind

```python
def bind(**context: Any) -> Logger: ...
```

The logger of the same stream, buffer and console, its context these fields besides its own.

### Logger.flush

```python
async def flush() -> None: ...
```

Hands what the buffer holds to the store now, as the timer does every second.

### Logger.stop

```python
async def stop() -> None: ...
```

Hands over what the logger holds and stops its timer, as the store's close does.

### Logger.take_record

```python
def take_record(record: logging.LogRecord, formatter: logging.Formatter) -> None: ...
```

Prints and keeps a logging record: its extra its fields, its logger's name in its context.

### Logger.take

```python
def take(line: Line) -> None: ...
```

Prints and keeps a line made elsewhere, a logging record's: from any thread, never waiting.

## logger

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
) -> Logger: ...
```

A logger of the console alone, for a program that wants the logger and no store:

    log = tinystore.logger("app", redact=tinystore.secrets)

``store.logger("app")`` in its place keeps every line too.

## trace

```python
@contextmanager
def trace(*, trace_id: str, span_id: str | None = None) -> Generator[None]: ...
```

Runs the block in a trace: 32 hex digits, and a span's 16.

## Line

```python
@dataclass(slots=True)
class Line:
    at: int  # unix nanoseconds
    stream: str
    level: int | None = None  # absent for an event
    event: str | None = None
    msg: str | None = None
    context: list[tuple[str, str]] = field(default_factory=_no_fields)
    attrs: list[tuple[str, str]] = field(default_factory=_no_fields)
    trace_id: str | None = None  # 32 hex digits
    span_id: str | None = None  # 16 hex digits
```

A line as a console writes it and the store keeps it: its fields already JSON.

<!-- Generated by task reference from sdk/python/src/tinystore/records.py, sdk/python/src/tinystore/logger.py, sdk/python/src/tinystore/trace.py, sdk/python/src/tinystore/_console.py. Edit the doc comments there, not this file. -->
