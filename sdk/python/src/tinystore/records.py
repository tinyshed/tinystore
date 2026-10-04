"""The application's logs and events in records.db, as Go's records keeps them.

One log of the store's whose calls name their streams, read by time and by
what records hold, followed in the order they were sealed; and a
logging.Handler that never makes its caller wait.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import re
import threading
import time
from collections import deque
from contextlib import contextmanager
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from ._background import DropNotice, FailureLog
from ._connection import Connection, Link, download
from ._console import Console, ConsoleHandler, ConsoleTime, Line
from ._page import Page
from ._time import Duration, ms, unix_ns
from ._trace import carried as _carried
from ._trace import shared as _shared
from ._values import to_json
from ._wire.messages import (
    METHODS,
    Empty,
    RecordsBatch,
    RecordsCursor,
    RecordsDamage,
    RecordsDamages,
    RecordsPage,
    RecordsQuery,
    RecordsRecord,
    RecordsStream,
)
from .errors import InvalidError

if TYPE_CHECKING:
    from collections.abc import AsyncIterator, Awaitable, Callable, Generator, Iterable, Mapping, Sequence
    from datetime import datetime
    from typing import TextIO

    from ._session import Stream

type Fields = Mapping[str, Any] | Sequence[tuple[str, Any]]
"""Keys and values, each value written as JSON; pairs may repeat a key."""

_LEVELS = {"debug": -4, "info": 0, "warn": 4, "warning": 4, "error": 8}
_FLUSH_EVERY = 1.0
"""seconds between a handler's writes, unless half its buffer waits first"""


def _level(level: str | int | None) -> int | None:
    if level is None or isinstance(level, int):
        return level
    return _LEVELS[level.lower()]


def _fields(fields: Fields | None) -> list[str] | None:
    if not fields:
        return None
    pairs = fields.items() if hasattr(fields, "items") else fields  # type: ignore[union-attr]
    flat: list[str] = []
    for key, value in pairs:  # type: ignore[misc]
        flat += [key, to_json(value)]
    return flat


def _id(given: bytes | str | None, size: int, what: str) -> bytes | None:
    if given is None:
        return None
    raw = bytes.fromhex(given) if isinstance(given, str) else given
    if len(raw) != size:
        raise InvalidError(f"a {what} id of {len(raw)} bytes, not {size}")
    return raw


def _text(t: str | bytes | None) -> str:
    return "" if t is None else t if isinstance(t, str) else t.decode(errors="replace")


@dataclass(frozen=True, slots=True)
class Record:
    """A record as it was kept; a field's value is the JSON it was written as, spelled as given."""

    at: int
    """unix nanoseconds"""
    stream: str
    name: str
    level: int | None
    body: str | bytes | None
    trace_id: bytes | None
    span_id: bytes | None
    context: list[tuple[str, str | bytes]]
    attrs: list[tuple[str, str | bytes]]


def fields(pairs: Iterable[tuple[str, str | bytes]]) -> dict[str, Any]:
    """A record's fields, its attrs or its context, as a dict, each value as json.loads reads it.

    The last of a repeated key is kept, and bytes that are not UTF-8 read
    with U+FFFD in their place. The pairs keep what json.loads may not:
    1.2300 as written.

        async for record in store.records.all(since="1h", streams=["readers"]):
            page = tinystore.fields(record.attrs)["page"]
    """
    return {
        key: json.loads(value if isinstance(value, str) else value.decode(errors="replace")) for key, value in pairs
    }


def _record(r: dict[str, Any]) -> Record:
    def pairs(flat: list[Any] | None) -> list[tuple[str, str | bytes]]:
        flat = flat or []
        return [(_text(flat[i]), flat[i + 1]) for i in range(0, len(flat) - 1, 2)]

    return Record(
        r.get("at", 0),
        _text(r.get("stream")),
        _text(r.get("name")),
        r.get("level"),
        r.get("body"),
        r.get("trace_id"),
        r.get("span_id"),
        pairs(r.get("context")),
        pairs(r.get("attrs")),
    )


@contextmanager
def trace(trace_id: bytes | str, span_id: bytes | str | None = None) -> Generator[None]:
    """Runs a block in a trace, as Go's records.WithTrace carries one in a context.

    A handler's lines, and records appended without a trace_id of their own,
    inside the block and in the tasks it starts, take its trace and span::

        with tinystore.trace(trace_id, span_id):
            log.info("charged")  # a record of that trace and span
    """
    carried = _id(trace_id, 16, "trace")
    if carried is None:
        raise InvalidError("a trace of no id")
    token = _carried.set((carried, _id(span_id, 8, "span")))
    try:
        yield
    finally:
        _carried.reset(token)


@contextmanager
def context(**fields: object) -> Generator[None]:
    """Runs a block whose log lines carry fields in their context, as Bun's log.with and Go's Logger.With do.

    A handler's lines inside the block, and in the tasks it starts, take the
    fields after their logger's name; an inner block adds its own, a field
    named again taking its newer value::

        with tinystore.context(request_id=request_id):
            log.info("charged", extra={"amount": 10})  # request_id in the line's context
    """
    token = _shared.set(tuple({**dict(_shared.get()), **fields}.items()))
    try:
        yield
    finally:
        _shared.reset(token)


_CURSOR = re.compile(r"(-?\d*):(-?\d*)")


def _cursor(start: int | None, end: int | None) -> str:
    """A page's next: where the range moved, both ends as nanoseconds.

    A query over the last ``since`` continues the range it started with::

        from 1700000000000000000, open end   ->   "1700000000000000000:"
    """
    return f"{'' if start is None else start}:{'' if end is None else end}"


def _range(
    since: Duration | None, from_: datetime | int | None, to: datetime | int | None, after: str | None
) -> tuple[int | None, int | None]:
    if after is not None:
        ends = _CURSOR.fullmatch(after)
        if ends is None:
            raise InvalidError(f"{after!r} is no page's next")
        start, end = ends.groups()
        return (int(start) if start else None), (int(end) if end else None)
    if since is not None and from_ is not None:
        raise InvalidError("a range starts since a span before now or from a time, not both")
    if since is not None:
        return time.time_ns() - ms(since) * 1_000_000, _nanos(to)
    return _nanos(from_), _nanos(to)


def _nanos(t: datetime | int | None) -> int | None:
    return None if t is None else unix_ns(t)


@dataclass(frozen=True, slots=True)
class Cursor:
    """Where a follower stands in the sealed segments, kept by the caller between follows."""

    segment: int = 0
    row: int = 0


class Records:
    def __init__(self, link: Link) -> None:
        self._link = link
        self._handlers: list[Handler] = []

    async def append(self, *records: Mapping[str, Any]) -> None:
        """Appends records in one transaction, all or none; a refused one names itself as call.

        A record is a mapping of at (a datetime or unix nanoseconds, now when
        absent), stream, name, level, body, trace_id, span_id, context, attrs.
        A record of no trace_id takes the trace the call runs in, from trace().
        """
        carried = _carried.get()
        if carried is not None:
            records = tuple(
                {**r, "trace_id": carried[0], "span_id": carried[1]} if r.get("trace_id") is None else r
                for r in records
            )
        await self._write(records)

    async def _write(self, records: Iterable[Mapping[str, Any]]) -> None:
        """Appends records as they are: a handler's, whose traces were taken as each line was logged."""
        now = time.time_ns()
        batch = [
            {
                "at": now if r.get("at") is None else unix_ns(r["at"]),
                "stream": r["stream"],
                "name": r["name"],
                "level": _level(r.get("level")),
                "body": r.get("body"),
                "trace_id": _id(r.get("trace_id"), 16, "trace"),
                "span_id": _id(r.get("span_id"), 8, "span"),
                "context": _fields(r.get("context")),
                "attrs": _fields(r.get("attrs")),
            }
            for r in records
        ]

        await self._send(batch)

    async def _write_lines(self, lines: Iterable[Line]) -> None:
        """Appends a handler's lines, their fields already JSON and their traces taken as each was logged."""
        await self._send(
            [
                {
                    "at": line.at,
                    "stream": line.stream,
                    "name": line.event or "log",
                    "level": line.level,
                    "body": line.msg,
                    "trace_id": None if line.trace_id is None else bytes.fromhex(line.trace_id),
                    "span_id": None if line.span_id is None else bytes.fromhex(line.span_id),
                    "context": [part for pair in line.context for part in pair] or None,
                    "attrs": [part for pair in line.attrs for part in pair] or None,
                }
                for line in lines
            ]
        )

    async def _send(self, batch: list[dict[str, Any]]) -> None:
        async def attempt(connection: Connection) -> None:
            await connection.session.call(METHODS["records.append"], RecordsBatch.encode(records=batch))

        await self._link.run("write", attempt)

    async def scan(
        self,
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
    ) -> Page[Record, str]:
        """One page of the records a query matches, from one snapshot, in event-time order.

        Over the last ``since``, or from ``from_`` to ``to``, in datetimes or
        unix nanoseconds. The page's next, while a limit or a budget ended it
        early, is passed back as ``after`` with the same query::

            page = await store.records.scan(since="1h", min_level="warn")
            more = await store.records.scan(since="1h", min_level="warn", after=page.next)
        """
        start, end = _range(since, from_, to, after)
        query = {
            "from_": start,
            "to": end,
            "streams": list(streams) if streams else None,
            "names": list(names) if names else None,
            "min_level": _level(min_level),
            "trace_id": _id(trace_id, 16, "trace"),
            "attrs": _fields(attrs),
            "context": _fields(context),
            "search": search or None,
            "newest": True if newest else None,
            "limit": limit,
        }

        async def attempt(connection: Connection) -> Any:
            return await download(connection, METHODS["records.read"], RecordsQuery.encode(query))

        got = await self._link.run("read", attempt)
        page = RecordsPage.decode(got.trailer)
        records = [_record(RecordsRecord.decode(i)) for i in got.items]
        if not page.get("more"):
            return Page(records, None)
        return Page(records, _cursor(page.get("from_", start), page.get("to", end)))

    async def all(
        self,
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
    ) -> AsyncIterator[Record]:
        """Every record a query matches, a page at a time."""
        start, end = _range(since, from_, to, None)
        query: dict[str, Any] = {
            "streams": streams,
            "names": names,
            "min_level": min_level,
            "trace_id": trace_id,
            "attrs": attrs,
            "context": context,
            "search": search,
            "newest": newest,
            "limit": limit,
        }
        after: str | None = _cursor(start, end)
        while after is not None:
            records, after = await self.scan(**query, after=after)
            for record in records:
                yield record

    async def follow(self, cursor: Cursor | None = None, limit: int | None = None) -> tuple[list[Record], Cursor, int]:
        """The sealed records after a cursor, the cursor to follow from next, and the segments expired first."""
        at = cursor or Cursor()

        async def attempt(connection: Connection) -> Any:
            body = RecordsCursor.encode(segment=at.segment or None, row=at.row or None, limit=limit)
            return await download(connection, METHODS["records.follow"], body)

        got = await self._link.run("read", attempt)
        trailer = RecordsCursor.decode(got.trailer)
        records = [_record(RecordsRecord.decode(i)) for i in got.items]
        return (
            records,
            Cursor(trailer.get("segment", 0), trailer.get("row", 0)),
            trailer.get("expired", 0),
        )

    def lines(self, stream: str, *, buffer: int = 1 << 20) -> Lines:
        """A writer of another program's output that never waits, dropping and counting what does not fit."""
        if not stream:
            raise InvalidError("lines of no stream")
        return Lines(self._link, stream, buffer)

    def handler(
        self,
        stream: str,
        level: int = logging.NOTSET,
        *,
        buffer: int = 1024,
        console: Console | None = None,
        time: ConsoleTime | None = None,
        hide_stream: bool = False,
        to: TextIO | None = None,
        redact: Iterable[str] = (),
    ) -> Handler:
        """A logging.Handler whose lines reach the console as they are logged and the stream once a second.

        It never makes the logger wait. console is pretty on a terminal and
        JSON otherwise when None, "off" for none; redact hides the values of
        fields of those names, at any depth, the case ignored, in the store
        and on the console.
        """
        handler = Handler(
            self._write_lines,
            stream,
            level,
            buffer,
            console=console,
            time=time,
            hide_stream=hide_stream,
            to=to,
            redact=redact,
        )
        self._handlers.append(handler)
        return handler

    async def damaged(self) -> list[dict[str, Any]]:
        """The rows the server's records have met that no longer read."""

        async def attempt(connection: Connection) -> bytes:
            return await connection.session.call(METHODS["records.damaged"], Empty.encode())

        return RecordsDamages.decode(await self._link.run("read", attempt)).get("damages", [])

    async def drop(self, damage: Mapping[str, Any]) -> None:
        """Removes a damaged row, a repair an admin connection alone may make."""

        async def attempt(connection: Connection) -> None:
            await connection.session.call(METHODS["records.drop"], RecordsDamage.encode(damage))

        await self._link.run("write", attempt)

    async def stop(self) -> None:
        for handler in self._handlers:
            await handler.flush_now()


class Lines:
    """An upload of lines that stays open while it is written; write returns at once."""

    def __init__(self, link: Link, stream: str, most: int) -> None:
        self._link, self._stream, self._most = link, stream, most
        self._waiting: deque[bytes] = deque()
        self._held = 0
        self._upload: Stream | None = None
        self._pumping: asyncio.Task[None] | None = None
        self.dropped = 0
        """bytes dropped since the writer began: a full buffer, or a lost connection"""

    def write(self, chunk: str | bytes) -> bool:
        data = chunk.encode() if isinstance(chunk, str) else chunk
        if not data:
            return True
        if self._held + len(data) > self._most:
            self.dropped += len(data)
            return False
        for at in range(0, len(data), 64 << 10):
            self._waiting.append(data[at : at + (64 << 10)])
        self._held += len(data)
        if self._pumping is None or self._pumping.done():
            self._pumping = asyncio.ensure_future(self._pump())
        return True

    async def _pump(self) -> None:
        while self._waiting:
            piece = self._waiting[0]
            try:
                if self._upload is None or self._upload.finished:
                    connection = await self._link.connection()
                    self._upload = await connection.session.open(
                        METHODS["records.lines"], RecordsStream.encode(stream=self._stream), False
                    )
                await self._upload.send(piece, False)
            except Exception:
                self._upload = None
                self.dropped += self._held
                self._waiting.clear()
                self._held = 0
                return
            self._waiting.popleft()
            self._held -= len(piece)

    async def end(self) -> None:
        """Hands over what the writer holds, a line cut short included, and ends the upload."""
        if self._pumping is not None:
            await self._pumping
        if self._upload is not None and not self._upload.finished:
            await self._upload.send(b"", True)
            await self._upload.next()


class Handler(ConsoleHandler):
    """A logging.Handler: emit writes the console line and queues the record.

    The queue is appended once a second, or as soon as half of it waits, so a
    burst is written rather than dropped; what does not fit, or a write that
    fails, is dropped and counted, and said in TinyStore's own lines.
    """

    def __init__(
        self,
        write: Callable[[Iterable[Line]], Awaitable[None]],
        stream: str,
        level: int,
        most: int,
        *,
        console: Console | None,
        time: ConsoleTime | None,
        hide_stream: bool,
        to: TextIO | None,
        redact: Iterable[str],
    ) -> None:
        super().__init__(stream, level, console=console, time=time, hide_stream=hide_stream, to=to, redact=redact)
        self._write, self._most = write, most
        self._queue: deque[Line] = deque()
        self._queued = threading.Lock()
        self._asked = False
        """a flush was asked for before its interval, and has not yet emptied the queue"""
        self._writing = asyncio.Lock()
        self._loop = asyncio.get_running_loop()
        self._task = self._loop.create_task(self._flushing())
        self._soon: set[asyncio.Task[None]] = set()
        """the flushes asked for before their interval, held since the loop holds a task weakly"""
        self._failures = FailureLog(f"records flush {stream}")
        self._drops = DropNotice(stream, most)
        self.dropped = 0
        """lines dropped since the handler began: a full buffer, or a write that failed"""

    def emit(self, record: logging.LogRecord) -> None:
        try:
            line = self._line(record)
            self._print(line)
        except Exception:
            self.handleError(record)
            return
        with self._queued:
            if len(self._queue) >= self._most:
                self.dropped += 1
                self._drops.dropped()
                return
            self._queue.append(line)
            ask = not self._asked and len(self._queue) >= self._most / 2
            self._asked = self._asked or ask
        if ask:
            self._ask_for_flush()

    def _ask_for_flush(self) -> None:
        # emit runs on whichever thread logged, the flush on the loop's; a loop
        # already closed has nobody to write for
        with contextlib.suppress(RuntimeError):
            self._loop.call_soon_threadsafe(self._flush_soon)

    def _flush_soon(self) -> None:
        task = self._loop.create_task(self.flush_now())
        self._soon.add(task)
        task.add_done_callback(self._soon.discard)

    async def _flushing(self) -> None:
        while True:
            await asyncio.sleep(_FLUSH_EVERY)
            await self.flush_now()

    async def flush_now(self) -> None:
        async with self._writing:
            with self._queued:
                batch = list(self._queue)
                self._queue.clear()
                self._asked = False
            self._drops.say_if_due()
            if not batch:
                return
            try:
                await self._write(batch)
            except Exception as err:
                with self._queued:
                    self.dropped += len(batch)
                self._failures.failed(err)
            else:
                self._failures.succeeded()

    def close(self) -> None:
        self._task.cancel()
        super().close()
