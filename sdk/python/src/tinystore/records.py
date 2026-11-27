"""Streams of events, as docs/guides/records/README.md has them.

A stream opened by name, its events appended whole and read back by time and
by what they hold, a page at a time, and every stream read together. A
store's logger keeps its lines in a stream through the same append; a tail
follows what is written from now on, and a writer of lines turns another
program's output into events::

    audit = store.events("audit", keep="365d")
    await audit.append("role.changed", {"user": 42, "role": "admin"})
    page = await store.events().list(since="1h", min_level="warn")
"""

from __future__ import annotations

import asyncio
import json
import re
from collections.abc import Mapping
from dataclasses import dataclass, field
from datetime import UTC, date, datetime, timedelta
from typing import TYPE_CHECKING, Any, Literal, Self, TypedDict, Unpack, cast, overload

from ._connection import check_name, piece_of
from ._console import Line, encode_fields
from ._fields import refuse_unknown
from ._handles import BoundStatement, Home, Opener, Via, handle_on, via_link
from ._time import Calendar, Duration, ms, spelled_keep
from ._values import to_json
from ._wire.protocol import (
    Empty,
    Handle,
    RecordsAggregateAsked,
    RecordsAppend,
    RecordsAppended,
    RecordsBatch,
    RecordsCount,
    RecordsDamage,
    RecordsDamages,
    RecordsDropped,
    RecordsEvents,
    RecordsFieldNamesAsked,
    RecordsFieldsAsked,
    RecordsLargestAsked,
    RecordsNames,
    RecordsOpen,
    RecordsPage,
    RecordsPiece,
    RecordsPollAsked,
    RecordsQuery,
    RecordsRows,
    RecordsSessions,
    RecordsSessionsAsked,
    RecordsStreamFields,
    RecordsStreams,
    RecordsTailBatch,
    RecordsTopAsked,
    RecordsTops,
    methods,
)
from .errors import ClosedError, InvalidError
from .trace import current_trace

if TYPE_CHECKING:
    from collections.abc import AsyncGenerator, AsyncIterator, Awaitable, Callable, Sequence
    from types import TracebackType

    from ._connection import Connection, Idempotence, Link
    from ._session import Stream

EventLevel = Literal["trace", "debug", "info", "warn", "error", "fatal"]
"""An event's level: what loggers and the programs whose lines are kept write."""

_LEVEL_NUMBERS: dict[str, int] = {"trace": -8, "debug": -4, "info": 0, "warn": 4, "warning": 4, "error": 8, "fatal": 12}
_LEVEL_NAMES: tuple[EventLevel, ...] = ("trace", "debug", "info", "warn", "error", "fatal")
_EPOCH = datetime(1970, 1, 1, tzinfo=UTC)

_LINES_HELD = 1 << 20
"""What a writer of lines holds for the store at most: past it, lines are dropped and counted."""
_EVENT_ROOM = 256 << 10
"""The bytes one event holds: a longer line is dropped whole, as the store drops one."""


@dataclass(frozen=True, slots=True)
class Event:
    """An event: what an append writes, and what a read gives back.

    A read fills every field; an append gives a name and what it knows, its
    time now unless given, from what the stream keeps to ten minutes ahead.
    """

    name: str
    """what happened; log for a logger's line"""
    fields: Mapping[str, Any] = field(default_factory=dict[str, Any])
    """what happened, any object JSON writes"""
    at: datetime | None = None
    context: Mapping[str, Any] = field(default_factory=dict[str, Any])
    """what it happened in: who asked, which request"""
    level: EventLevel | None = None
    message: str | None = None
    trace_id: str | None = None
    """32 hex digits: the current trace's unless given"""
    span_id: str | None = None
    """16 hex digits"""
    stream: str | None = None
    """the stream a read found it in"""
    id: str | None = None
    """what names it in its stream, 1 to 128 bytes: an append of an id the stream holds writes nothing"""


@dataclass(frozen=True, slots=True)
class EventPage:
    """A page of events, and the page after it; the last page has none."""

    events: list[Event]
    next: str | None


@dataclass(frozen=True, slots=True)
class Top:
    """One of a key's most frequent values, and how many events hold it."""

    value: Any
    count: int


@dataclass(frozen=True, slots=True)
class Stats:
    """A field's numbers in a row: samples are the numbers, which avg divides sum by."""

    samples: int
    sum: int | float
    min: int | float
    max: int | float
    avg: float


@dataclass(frozen=True, slots=True)
class Session:
    """A session: the events of one value of each key of by, each within gap of the one before."""

    by: dict[str, Any]
    from_: datetime
    """its first event's time"""
    to: datetime
    """its last event's time"""
    count: int
    """every event of it, however few it keeps"""
    events: list[Event]
    """its first events, as many as max_events asked"""
    partial: bool
    """whether it may go on outside what was read: its first event within gap of the range's start, or its last
    within gap of its end"""


@dataclass(frozen=True, slots=True)
class Row:
    """A row of an aggregate: a span of every and a value of each key of by, with its count."""

    at: datetime
    """where the row's span begins, counted from 1970 in UTC"""
    count: int
    by: dict[str, Any]
    stats: dict[str, Stats]
    distinct: dict[str, int]
    date: date | None = None
    """the local date a calendar's row begins on: a week's Monday, a month's first; None without a calendar"""


@dataclass(frozen=True, slots=True)
class StreamInfo:
    """A stream with events: what it keeps, where that begins, and its newest event's time."""

    name: str
    keep: str
    retained_from: datetime
    """its oldest event, or where its keep begins while it holds older"""
    newest: datetime


ValueKind = Literal["float", "integer", "text", "bool", "object", "list", "null"]
"""What a value is, as its JSON is spelled: ``float`` has a fraction or an exponent, ``integer`` is whole."""


@dataclass(frozen=True, slots=True)
class NameCount:
    """A name a stream's events have, and how many have it."""

    name: str
    count: int


@dataclass(frozen=True, slots=True)
class FieldInfo:
    """A key of a stream's events' fields or contexts."""

    name: str
    kinds: list[ValueKind]
    """the kinds of value it holds, in the order ``ValueKind`` lists them"""
    count: int
    """the events that carry it: one that writes it twice, once"""


@dataclass(frozen=True, slots=True)
class StreamFields:
    """What a stream's events carry, each by name."""

    event_names: list[NameCount]
    fields: list[FieldInfo]
    context: list[FieldInfo]


@dataclass(frozen=True, slots=True)
class PollBatch:
    """Every event once, a batch a call: its events, where the next poll begins, and what expired before it."""

    events: list[Event]
    cursor: str
    """where the next poll begins, kept once these events are safe"""
    expired: int
    """the events keep removed before the cursor reached them"""


@dataclass(frozen=True, slots=True)
class TailBatch:
    """A tail's batch: the events written since the last, and how many it missed."""

    events: list[Event]
    missed: int


@dataclass(frozen=True, slots=True)
class Damage:
    """A head row or a segment that no longer reads, as damaged lists it and drop takes it."""

    part: Literal["head row", "segment"]
    id: int
    stream: str
    first: datetime
    last: datetime
    error: str


class TailQuery(TypedDict, total=False):
    """What a tail finds: every condition given holds; one given as None is InvalidError."""

    streams: Sequence[str]
    """any of these streams, for every stream's read"""
    names: Sequence[str]
    """any of these names: ["log"] is the lines"""
    min_level: EventLevel
    """this level and worse; an event without a level is not found"""
    trace_id: str
    where: Mapping[str, Any]
    """each key, in fields or in context, with this value by the JSON it was written as; one of a
    list, [7, 12]; none of them with {"not": …}, the key absent too; or a text that starts with
    {"prefix": …}. A list or a dict as the value itself goes inside a list: [["a", "b"]]."""
    search: str
    """this text in the message, the name or a value, its case ignored"""


class Counted(TailQuery, total=False):
    """What a count, a top or an aggregate finds: a tail's conditions and a range."""

    since: Duration
    """the span before now, "1h"; or from_ and to"""
    from_: datetime
    """the range's start, included"""
    to: datetime
    """the range's end, excluded"""


class Query(Counted, total=False):
    """What a list finds, and how its page reads."""

    newest: bool
    """newest first; oldest first without it"""
    limit: int
    """the events of a page: 1000, and 10,000 at most"""
    after: str
    """a page's next, with the rest of the query as it was"""


class Named(Counted, total=False):
    limit: int
    prefix: str


class Topped(Counted, total=False):
    limit: int


class Sessioned(Counted, total=False):
    max: Duration
    """the longest a session lasts: an event later than this after its first begins the next"""
    first: int
    """each value's first sessions of the range alone, its later events passed over"""
    events: int
    """the first events each session keeps, 1000 at most; none unless asked"""


class Aggregated(Counted, total=False):
    every: Duration
    """the span of a row: the range is one without it"""
    calendar: Calendar
    """rows of a calendar in time_zone, in place of every: "day" begins each at a local midnight"""
    time_zone: str
    """the IANA zone a calendar's rows are in, such as Europe/Moscow or UTC"""
    by: Sequence[str]
    """keys whose every value has rows of its own"""
    stats: Sequence[str]
    """fields whose numbers a row sums"""
    distinct: Sequence[str]
    """fields whose different values a row counts"""
    fill: bool
    """a row for every span of the range the stream keeps, an empty one counting 0; it needs since or from_"""


def _nanos(at: datetime, what: str) -> int:
    if at.tzinfo is None:
        raise InvalidError(f"{what} {at.isoformat()} has no zone: give it one, as datetime.now(UTC) does")
    delta = at - _EPOCH
    return (delta.days * 86_400 + delta.seconds) * 1_000_000_000 + delta.microseconds * 1000


def _moment(nanos: int) -> datetime:
    return _EPOCH + timedelta(microseconds=nanos // 1000)


def _level_number(level: str | int, what: str) -> int:
    if isinstance(level, int) and not isinstance(level, bool):
        return level
    found = _LEVEL_NUMBERS.get(str(level))
    if found is None:
        raise InvalidError(f"{what}: the level {level!r}, not one of {', '.join(_LEVEL_NAMES)}")
    return found


_HEX = re.compile(r"[0-9a-fA-F]*")


def _id_bytes(hex_id: str, size: int, what: str) -> bytes:
    if not isinstance(hex_id, str) or len(hex_id) != 2 * size or not _HEX.fullmatch(hex_id):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"{what} {hex_id!r}: {2 * size} hex digits")
    return bytes.fromhex(hex_id)


_QUERY_FIELDS = Query.__required_keys__ | Query.__optional_keys__
"""The fields of a query, which a call takes on top of its own."""


class _Asked(dict[str, Any]):
    """A query as the wire carries it, and the statements its conditions name by their place.

    Each statement's database is a handle of the connection the query goes on, which ``on`` writes in.
    """

    bound: list[BoundStatement]

    async def on(self, connection: Connection) -> None:
        """Writes in the statements, each with its database's handle on ``connection``."""
        if self.bound:
            self["statements"] = [
                {"database": await each.database.handle(connection), "text": each.text, "values": list(each.values)}
                for each in self.bound
            ]


def _refuse_statements(link: Link, asked: _Asked) -> None:
    """Refuses a statement of another store's database.

    Its database's handle would be opened on this store's connection, and read this store's file of the same name.
    """
    for each in asked.bound:
        if getattr(each.database, "_link", None) is not link:
            raise InvalidError(
                f"{each.database.label}: a statement of another store's database, in a query of this one"
            )


def _query_of(query: Mapping[str, Any]) -> _Asked:
    """A query as the wire carries it.

    A condition given as None reads nothing it was not asked, and a field no query has is refused by name, since one
    left out would read every event.
    """
    refuse_unknown("records query", query, _QUERY_FIELDS)
    for key, value in query.items():
        if value is None:
            raise InvalidError(f"a query's {key} given as None: leave it out to read without it")
    bound: list[BoundStatement] = []
    where, conditions = _where_of(query.get("where") or {}, bound)
    asked = _Asked(
        {
            "since": None if "since" not in query else ms(query["since"], "a query's since"),
            "from": None if "from_" not in query else _nanos(query["from_"], "a query's from_"),
            "to": None if "to" not in query else _nanos(query["to"], "a query's to"),
            "streams": None if "streams" not in query else list(query["streams"]),
            "names": None if "names" not in query else list(query["names"]),
            "min_level": None if "min_level" not in query else _level_number(query["min_level"], "min_level"),
            "trace_id": None if "trace_id" not in query else _id_bytes(query["trace_id"], 16, "trace_id"),
            "where": where or None,
            "conditions": conditions or None,
            "search": query.get("search"),
            "newest": query.get("newest"),
            "limit": query.get("limit"),
            "after": query.get("after"),
        }
    )
    asked.bound = bound
    return asked


def _where_of(
    given: Mapping[str, Any], bound: list[BoundStatement]
) -> tuple[dict[str, str], dict[str, dict[str, Any]]]:
    """A where as the wire carries it: a plain value as its JSON in where, which every server
    reads, and a list, {"not": …}, {"prefix": …} or a statement in conditions, which a server older
    than them refuses rather than matching nothing. A statement goes into bound, which the
    condition names by its place."""
    from .sql import Sql  # sql imports records: a piece is named here only to say what to use instead

    where: dict[str, str] = {}
    conditions: dict[str, dict[str, Any]] = {}
    for key, value in given.items():
        named = f"a query's where {key!r}"
        if value is None:
            raise InvalidError(f"{named} given as None")
        if isinstance(value, BoundStatement):
            bound.append(value)
            conditions[key] = {"statement": len(bound) - 1}
        elif isinstance(value, Sql):
            why = "a stream belongs to no database, so its statement says whose it is: db.statement(…)"
            raise InvalidError(f"{named}: {why}")
        elif isinstance(value, (list, tuple)):
            conditions[key] = {"values": [to_json(item) for item in cast("Sequence[Any]", value)]}
        elif isinstance(value, Mapping):
            conditions[key] = _condition_of(named, cast("Mapping[str, Any]", value), bound)
        else:
            where[key] = to_json(value)
    return where, conditions


def _condition_of(named: str, condition: Mapping[str, Any], bound: list[BoundStatement]) -> dict[str, Any]:
    if list(condition) == ["not"] and isinstance(condition["not"], BoundStatement):
        bound.append(condition["not"])
        return {"statement": len(bound) - 1, "not": True}
    if list(condition) == ["not"]:
        nots = condition["not"]
        listed = list(cast("Sequence[Any]", nots)) if isinstance(nots, (list, tuple)) else [nots]
        return {"values": [to_json(item) for item in listed], "not": True}
    if list(condition) == ["prefix"] and isinstance(condition["prefix"], str):
        return {"prefix": condition["prefix"]}
    raise InvalidError(f"{named}: a value, a list, not or prefix; a dict as the value itself goes in a list, [{{…}}]")


def _event_of(read: dict[str, Any]) -> Event:
    level = read.get("level")
    trace, span = read.get("trace_id"), read.get("span_id")
    return Event(
        name=read.get("name", ""),
        fields=json.loads(read["fields"]) if "fields" in read else {},
        at=_moment(read.get("at", 0)),
        context=json.loads(read["context"]) if "context" in read else {},
        level=None if level is None else _LEVEL_NAMES[min(5, max(0, level // 4 + 2))],
        message=read.get("message"),
        trace_id=None if trace is None else trace.hex(),
        span_id=None if span is None else span.hex(),
        stream=read.get("stream", ""),
        id=read.get("id"),
    )


def _object_of(pairs: Sequence[tuple[str, str]]) -> str | None:
    """Key and JSON pairs as the JSON object they spell; none for no field."""
    if not pairs:
        return None
    return "{" + ",".join(f"{json.dumps(key, ensure_ascii=False)}:{value}" for key, value in pairs) + "}"


def _wire_trace(trace_id: str | None, span_id: str | None) -> dict[str, bytes]:
    """A trace and its span as the wire carries them: both, or neither, since the store keeps a span with its trace."""
    if trace_id is None or span_id is None:
        return {}
    return {"trace_id": _id_bytes(trace_id, 16, "trace_id"), "span_id": _id_bytes(span_id, 8, "span_id")}


def _new_event(event: Event, place: int) -> dict[str, Any]:
    """An event as the wire carries it, the current trace's ids when it gives none."""
    named = f"event {place} {event.name!r}"
    if not isinstance(event.name, str) or not event.name:  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"{named}: an event has a name")
    if event.id is not None and (not isinstance(event.id, str) or not event.id):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"{named}: an id is a string of 1 to 128 bytes")
    traced = current_trace() if event.trace_id is None else (event.trace_id, event.span_id)
    return {
        "name": event.name,
        "id": event.id,
        "at": None if event.at is None else _nanos(event.at, f"{named}: its time"),
        "level": None if event.level is None else _level_number(event.level, named),
        "message": event.message,
        "fields": _object_of(encode_fields(event.fields.items(), None)) if event.fields else None,
        "context": _object_of(encode_fields(event.context.items(), None)) if event.context else None,
        **({} if traced is None else _wire_trace(*traced)),
    }


class _Reads:
    """What every read takes: a stream's handle, or every stream without one."""

    _link: Link

    async def _ask(self, method: str, body: Callable[[int | None], bytes], asked: _Asked | None = None) -> bytes:
        """Calls method with what body writes, given the stream's handle, once asked's statements are written in."""
        raise NotImplementedError

    async def _opened(self, method: str, body: Callable[[int | None], bytes]) -> Stream:
        raise NotImplementedError

    async def list(self, **query: Unpack[Query]) -> EventPage:
        """One page: EventPage(events, next), its limit of events that match, fewer only on the last, without a next."""
        asked = _query_of(query)
        page = RecordsPage.decode(
            await self._ask("records.list", lambda h: RecordsQuery.encode(asked, handle=h), asked)
        )
        return EventPage([_event_of(each) for each in page.get("events", [])], page.get("next"))

    async def all(self, **query: Unpack[Query]) -> AsyncIterator[Event]:
        """Every event the query finds, a page after another."""
        while True:
            page = await self.list(**query)
            for event in page.events:
                yield event
            if page.next is None:
                return
            query["after"] = page.next

    async def delete(self, **query: Unpack[Counted]) -> int:
        """Deletes every event the query finds, of those appended before the call, and answers how many.

        It takes a where, and no search: what the store keeps only for those events goes with them, a context of their
        own, their keys and counts, the bytes they lay in. It cannot be undone, and run again it deletes what is left:
        no poll's cursor moves, and an event's id stays held until the stream's keep passes its time. A backup taken
        before it, and another node's store, keep the events.

            await store.events().delete(streams=["activity"], where={"userId": 97516})
        """
        asked = _query_of(query)
        return RecordsCount.decode(
            await self._ask("records.delete", lambda h: RecordsQuery.encode(asked, handle=h), asked)
        ).get("count", 0)

    async def count(self, **query: Unpack[Counted]) -> int:
        """How many events the query finds."""
        asked = _query_of(query)
        return RecordsCount.decode(
            await self._ask("records.count", lambda h: RecordsQuery.encode(asked, handle=h), asked)
        ).get("count", 0)

    async def top(self, key: str, /, **query: Unpack[Topped]) -> list[Top]:
        """A key's most frequent values and how many events hold each: ten unless limit asks for more, 1000 at most.

        The key is a field's or a context's, or stream, name, level or message.
        """
        asked = _query_of(query)
        answer = await self._ask(
            "records.top", lambda h: RecordsTopAsked.encode(query={**asked, "handle": h}, key=key), asked
        )
        return [
            Top(json.loads(each.get("value", "null")), each.get("count", 0))
            for each in RecordsTops.decode(answer).get("values", [])
        ]

    async def largest(self, key: str, /, **query: Unpack[Topped]) -> list[Event]:
        """The events whose value at key is a number, the largest first: ten unless limit asks for more, 1000 at most.

        The key is a field's, else a context's; an event without a number there is passed over, and events of one
        number come in the order a list gives them.
        """
        asked = _query_of(query)
        answer = await self._ask(
            "records.largest", lambda h: RecordsLargestAsked.encode(query={**asked, "handle": h}, key=key), asked
        )
        return [_event_of(event) for event in RecordsEvents.decode(answer).get("events", [])]

    async def aggregate(self, **query: Unpack[Aggregated]) -> list[Row]:
        """A row for each span of every and each value of the keys of by, with its count."""
        every, by = query.pop("every", None), query.pop("by", None)
        calendar, time_zone = query.pop("calendar", None), query.pop("time_zone", None)
        stats, distinct = query.pop("stats", None), query.pop("distinct", None)
        fill = query.pop("fill", None)
        asked = _query_of(query)
        answer = await self._ask(
            "records.aggregate",
            lambda h: RecordsAggregateAsked.encode(
                query={**asked, "handle": h},
                every=None if every is None else ms(every, "an aggregate's every"),
                calendar=calendar,
                time_zone=time_zone,
                by=None if by is None else list(by),
                stats=None if stats is None else list(stats),
                distinct=None if distinct is None else list(distinct),
                fill=fill,
            ),
            asked,
        )
        return [
            Row(
                at=_moment(row.get("at", 0)),
                count=row.get("count", 0),
                by={key: json.loads(value) for key, value in row.get("by", {}).items()},
                stats={
                    key: Stats(
                        kept.get("samples", 0),
                        json.loads(kept.get("sum", "0")),
                        json.loads(kept.get("min", "0")),
                        json.loads(kept.get("max", "0")),
                        kept.get("avg", 0.0),
                    )
                    for key, kept in row.get("stats", {}).items()
                },
                distinct=dict(row.get("distinct", {})),
                date=None if "date" not in row else date.fromisoformat(row["date"]),
            )
            for row in RecordsRows.decode(answer).get("rows", [])
        ]

    async def sessions(self, *, by: str | Sequence[str], gap: Duration, **query: Unpack[Sessioned]) -> list[Session]:
        """The events the query finds cut into sessions by a pause, in the order of their first events.

        A session is the events of one value of each key of by, each within gap of the one before. The answer is whole,
        not paged, and LimitError past 10,000 sessions held, open ones among them, or 64 MiB of what they keep.

            await events.sessions(since="30d", by="userId", gap="30m", max="4h", first=1, events=50)
        """
        longest, first, events = query.pop("max", None), query.pop("first", None), query.pop("events", None)
        asked = _query_of(query)
        answer = await self._ask(
            "records.sessions",
            lambda h: RecordsSessionsAsked.encode(
                query={**asked, "handle": h},
                by=[by] if isinstance(by, str) else list(by),
                gap=ms(gap, "a session's gap"),
                max=None if longest is None else ms(longest, "a session's max"),
                first=first,
                events=events,
            ),
            asked,
        )
        return [
            Session(
                by={key: json.loads(value) for key, value in session.get("by", {}).items()},
                from_=_moment(session.get("from", 0)),
                to=_moment(session.get("to", 0)),
                count=session.get("count", 0),
                events=[_event_of(each) for each in session.get("events", [])],
                partial=session.get("partial", False),
            )
            for session in RecordsSessions.decode(answer).get("sessions", [])
        ]

    async def field_names(self, **query: Unpack[Named]) -> list[str]:
        """The keys the events hold, in fields and in context, sorted; prefix narrows them."""
        prefix = query.pop("prefix", None)
        asked = _query_of(query)
        answer = await self._ask(
            "records.fieldNames",
            lambda h: RecordsFieldNamesAsked.encode(query={**asked, "handle": h}, prefix=prefix),
            asked,
        )
        return list(RecordsNames.decode(answer).get("names", []))

    async def tail(self, **query: Unpack[TailQuery]) -> AsyncGenerator[TailBatch]:
        """The events written from now on that the query finds, a batch as they are written.

        A reader slower than the writers skips what left the store's hold, and
        the batch after says how many. It ends when its loop is left, and when
        the store closes.
        """
        asked = _query_of(query)
        if asked.bound:
            why = "a statement is read once, as a query starts, and a tail lives on"
            raise InvalidError(f"a tail's where takes no statement: {why}")
        stream = await self._opened("records.tail", lambda h: RecordsQuery.encode(asked, handle=h))
        unfollow = self._link.follow(stream)
        try:
            while True:
                event = await stream.next()
                stream.consumed(len(event.body))
                if event.end:
                    return
                batch = RecordsTailBatch.decode(event.body)
                yield TailBatch([_event_of(each) for each in batch.get("events", [])], batch.get("missed", 0))
        finally:
            stream.cancel()
            unfollow()


async def _stream_of(connection: Connection, method: int, body: bytes, end: bool) -> Stream:
    """A stream of the method, open once its RESPONSE came; end leaves the client nothing to send."""
    stream = await connection.session.open(method, body, end)
    await stream.next()
    return stream


def open_events(link: Link, stream: str, keep: Duration | None) -> Events:
    """Opens a stream of events: lazily, at its first call on a connection."""
    return Events(link, stream, events_open(stream, keep))


def events_open(stream: str, keep: Duration | None, database: int | None = None) -> bytes:
    """A stream's open, through the database of the handle database on the connection when given."""
    check_name(stream, "stream")
    return RecordsOpen.encode(
        stream=stream, keep=None if keep is None else ms(keep, "a stream's keep"), database=database
    )


class Events(_Reads):
    """A stream of events: written with append, which waits for the file, and read back."""

    def __init__(self, link: Link, stream: str, opener: Opener, home: Home | None = None) -> None:
        self.stream = stream
        self._link = link
        self._via = via_link(link) if home is None else home.via(f"events {stream}")
        self._opener = opener
        self.home = home
        """the database the stream was opened through, whose transactions and batches its appends join"""

    @property
    def opener(self) -> Opener:
        return self._opener

    def appending(
        self,
        name: str,
        fields: Mapping[str, Any] | None = None,
        /,
        *,
        id: str | None = None,
        at: datetime | None = None,
        context: Mapping[str, Any] | None = None,
        level: EventLevel | None = None,
        message: str | None = None,
        trace_id: str | None = None,
        span_id: str | None = None,
    ) -> Appending:
        """An event's append for a batch of the database the stream was opened from, ``db.events``.

        It is kept with the batch's rows and reaches the stream once they
        commit, before the batch returns, or never::

            changed = sql("update roles set role = ? where user_id = ?", role, user_id)
            await db.batch([changed, audit.appending("role.changed", {"user_id": user_id})])
        """
        event = Event(name, fields or {}, at, context or {}, level, message, trace_id, span_id, id=id)
        return Appending(self, _new_event(event, 0))

    @overload
    async def append(
        self,
        name_or_events: str,
        fields: Mapping[str, Any] | None = None,
        /,
        *,
        id: str | None = None,
        at: datetime | None = None,
        context: Mapping[str, Any] | None = None,
        level: EventLevel | None = None,
        message: str | None = None,
        trace_id: str | None = None,
        span_id: str | None = None,
    ) -> bool: ...

    @overload
    async def append(self, name_or_events: Sequence[Event], /) -> int: ...

    async def append(
        self,
        name_or_events: str | Sequence[Event],
        fields: Mapping[str, Any] | None = None,
        /,
        *,
        id: str | None = None,
        at: datetime | None = None,
        context: Mapping[str, Any] | None = None,
        level: EventLevel | None = None,
        message: str | None = None,
        trace_id: str | None = None,
        span_id: str | None = None,
    ) -> bool | int:
        """Writes an event, or a list of them whole or not at all.

        It returns once they are written, and a read finds them at once. An
        event whose id the stream holds, or an earlier event of the list
        has, is written as nothing: one event answers whether it was
        written, a list how many of its events were. A refused event names
        its place in the list and its name.
        """
        if isinstance(name_or_events, str):
            event = Event(name_or_events, fields or {}, at, context or {}, level, message, trace_id, span_id, id=id)
            events: Sequence[Event] = [event]
        else:
            events = name_or_events
        encoded = [_new_event(event, place) for place, event in enumerate(events)]
        answer = await self._call("records.append", lambda h: RecordsAppend.encode(handle=h, events=encoded), "write")
        written = len(events) - RecordsAppended.decode(answer).get("held", 0)
        return written == 1 if isinstance(name_or_events, str) else written

    async def append_lines(self, lines: list[Line]) -> None:
        """A logger's lines, each an event of this stream."""
        events = [
            {
                "name": line.event or "log",
                "at": line.at,
                "level": line.level,
                "message": line.msg,
                "fields": _object_of(line.attrs),
                "context": _object_of(line.context),
                **_wire_trace(line.trace_id, line.span_id),
            }
            for line in lines
        ]
        await self._call("records.append", lambda h: RecordsAppend.encode(handle=h, events=events), "write")

    def lines(self) -> Lines:
        """A writer of another program's output into this stream, each line an event at the time it arrived.

        A stack trace's or a JSON value's lines make one, and a write never waits::

            async with store.events("worker").lines() as out:
                async for chunk in child.stdout:
                    await out.write(chunk)
        """

        async def begin() -> tuple[Stream, int]:
            connection = await self._link.connection()
            handle = await handle_on(connection, methods["records.open"], self._opener)
            stream = await _stream_of(connection, methods["records.lines"], Handle.encode(handle=handle), False)
            return stream, piece_of(connection)

        return Lines(begin)

    async def _ask(self, method: str, body: Callable[[int | None], bytes], asked: _Asked | None = None) -> bytes:
        if asked is None or not asked.bound:
            return await self._call(method, body, "read")
        _refuse_statements(self._link, asked)
        refuse_around = getattr(self.home, "refuse_around", None)
        if refuse_around is not None:
            refuse_around(f"events {self.stream}")

        # the databases' handles are the connection's the call goes on, opened before it
        async def attempt(connection: Connection) -> bytes:
            await asked.on(connection)
            handle = await handle_on(connection, methods["records.open"], self._opener)
            return await connection.session.call(methods[method], body(handle))

        return await self._link.run("read", attempt)

    async def _opened(self, method: str, body: Callable[[int | None], bytes]) -> Stream:
        async def attempt(connection: Connection) -> Stream:
            handle = await handle_on(connection, methods["records.open"], self._opener)
            return await _stream_of(connection, methods[method], body(handle), True)

        return await self._link.run("read", attempt)

    async def _call(self, method: str, body: Callable[[int], bytes], idempotence: Idempotence) -> bytes:
        return await self._via.call(methods["records.open"], self._opener, methods[method], body, idempotence)


class TxEvents:
    """A stream's appends inside a transaction of the database it was opened from, ``tx.with_(audit)``.

    Each is kept with the transaction's rows and reaches the stream once it
    commits, before ``db.tx`` returns. A read goes through the stream itself,
    and finds what committed.
    """

    def __init__(self, events: Events, via: Via) -> None:
        self.stream = events.stream
        self._opener = events.opener
        self._via = via

    async def append(
        self,
        name: str,
        fields: Mapping[str, Any] | None = None,
        /,
        *,
        id: str | None = None,
        at: datetime | None = None,
        context: Mapping[str, Any] | None = None,
        level: EventLevel | None = None,
        message: str | None = None,
        trace_id: str | None = None,
        span_id: str | None = None,
    ) -> bool:
        """Appends an event once the transaction commits.

        It says whether it will be written: False when its stream, or this
        transaction, holds its id.
        """
        event = Event(name, fields or {}, at, context or {}, level, message, trace_id, span_id, id=id)
        encoded = [_new_event(event, 0)]
        answer = await self._via.call(
            methods["records.open"],
            self._opener,
            methods["records.append"],
            lambda handle: RecordsAppend.encode(handle=handle, events=encoded),
            "write",
        )
        return RecordsAppended.decode(answer).get("held", 0) == 0


@dataclass(frozen=True, slots=True)
class Appending:
    """An event's append that a database's batch carries, from ``events.appending``."""

    events: Events = field(repr=False)
    event: dict[str, Any] = field(repr=False)
    """the event as the wire carries it"""


def appending_in(appending: Appending, home: Home) -> Callable[[Connection], Awaitable[dict[str, Any]]]:
    """What a batch of home sends of an append, on the connection it goes on: its stream's handle opened there.

    A stream opened from the store or another database is refused, since its
    event would not commit with the rows.
    """
    events, event = appending.events, appending.event
    if events.home is not home:
        kept = "records.db" if events.home is None else events.home.label
        raise InvalidError(f"{home.label}: events {events.stream}: kept in {kept}, outside this batch of {home.label}")

    async def sent(connection: Connection) -> dict[str, Any]:
        handle = await handle_on(connection, methods["records.open"], events.opener)
        body = RecordsAppend.encode(handle=handle, events=[event])
        return {"text": "", "values": [], "method": methods["records.append"], "body": body}

    return sent


class AllEvents(_Reads):
    """Every stream's events, to read: ``store.events()``."""

    def __init__(self, link: Link) -> None:
        self._link = link

    async def streams(self) -> list[StreamInfo]:
        """Every stream with events, by name, with what it keeps, where that begins and its newest event's time."""
        answer = await self._ask("records.streams", lambda _: Empty.encode())
        return [
            StreamInfo(
                each.get("name", ""),
                spelled_keep(each.get("keep", 0)),
                _moment(each.get("retained_from", 0)),
                _moment(each.get("newest", 0)),
            )
            for each in RecordsStreams.decode(answer).get("streams", [])
        ]

    async def fields(self, stream: str) -> StreamFields:
        """What the events of ``stream`` carry, none of it read from a packed event.

        Each name with how many events have it, and each key of their fields
        and contexts with the kinds of value it holds and how many events
        carry it. A stream with no events carries nothing.
        """
        answer = await self._ask("records.fields", lambda _: RecordsFieldsAsked.encode(stream=stream))
        carried = RecordsStreamFields.decode(answer)

        def info(each: Mapping[str, Any]) -> FieldInfo:
            return FieldInfo(each.get("name", ""), list(each.get("kinds", [])), each.get("count", 0))

        return StreamFields(
            [NameCount(each.get("name", ""), each.get("count", 0)) for each in carried.get("event_names", [])],
            [info(each) for each in carried.get("fields", [])],
            [info(each) for each in carried.get("context", [])],
        )

    async def poll(self, cursor: str | None = None, *, limit: int | None = None) -> PollBatch:
        """Every event once, a batch a call, from a cursor the program keeps, the oldest event kept without one.

        With no event past the cursor, the batch is empty and its cursor the
        one given.
        """
        answer = await self._ask("records.poll", lambda _: RecordsPollAsked.encode(cursor=cursor, limit=limit))
        batch = RecordsBatch.decode(answer)
        return PollBatch(
            [_event_of(e) for e in batch.get("events", [])], batch.get("cursor", ""), batch.get("expired", 0)
        )

    async def damaged(self) -> list[Damage]:
        """What reads met that no longer reads, which drop removes: each head row and segment with its times."""
        answer = await self._ask("records.damaged", lambda _: Empty.encode())
        return [
            Damage(
                "segment" if each.get("part") == "segment" else "head row",
                each.get("id", 0),
                each.get("stream", ""),
                _moment(each.get("first", 0)),
                _moment(each.get("last", 0)),
                each.get("error", ""),
            )
            for each in RecordsDamages.decode(answer).get("damages", [])
        ]

    async def drop(self, damage: Damage) -> None:
        """Removes a head row or a segment that damaged listed, so that the reads over it work again."""
        body = RecordsDamage.encode(
            part=damage.part,
            id=damage.id,
            stream=damage.stream,
            first=_nanos(damage.first, "a damage's first"),
            last=_nanos(damage.last, "a damage's last"),
            error=damage.error,
        )
        await self._link.run("write", lambda c: c.session.call(methods["records.drop"], body))

    async def _ask(self, method: str, body: Callable[[int | None], bytes], asked: _Asked | None = None) -> bytes:
        if asked is not None:
            _refuse_statements(self._link, asked)

        async def attempt(connection: Connection) -> bytes:
            if asked is not None:
                await asked.on(connection)
            return await connection.session.call(methods[method], body(None))

        return await self._link.run("read", attempt)

    async def _opened(self, method: str, body: Callable[[int | None], bytes]) -> Stream:
        return await self._link.run("read", lambda c: _stream_of(c, methods[method], body(None), True))


class Lines:
    """A writer of another program's output into a stream, as ``events.lines()`` begins it.

    Each line becomes an event, and a write never waits. It sends whole
    lines, so that what it has no room for, dropped counts as lines.
    """

    def __init__(self, begin: Callable[[], Any]) -> None:
        self._begin = begin
        self._queued: list[bytes] = []
        self._queued_bytes = 0
        self._partial = b""
        self._discarding = False
        self._dropped = 0
        self._ended = False
        self._failure: BaseException | None = None
        self._wake = asyncio.Event()
        self._sending: asyncio.Task[None] | None = None

    @property
    def dropped(self) -> int:
        """The lines dropped: past what the writer holds, and, once it ended, by the store."""
        return self._dropped

    async def __aenter__(self) -> Self:
        stream, piece = await self._begin()
        self._sending = asyncio.ensure_future(self._send(stream, piece))
        return self

    async def __aexit__(
        self, kind: type[BaseException] | None, error: BaseException | None, trace: TracebackType | None
    ) -> None:
        await self.end()

    async def write(self, chunk: bytes | str) -> None:
        """Takes a piece of output, cut anywhere; it never waits."""
        if self._ended:
            raise ClosedError("lines written after their end")
        data = chunk.encode() if isinstance(chunk, str) else bytes(chunk)
        if self._discarding:
            end = data.find(b"\n")
            if end < 0:
                return
            self._discarding = False
            data = data[end + 1 :]
        joined = self._partial + data
        cut = joined.rfind(b"\n") + 1
        self._partial = joined[cut:]
        if len(self._partial) > _EVENT_ROOM:
            self._partial = b""
            self._discarding = True
            self._dropped += 1
        self._queue(joined[:cut])

    async def end(self) -> None:
        """Ends the writer: what it holds goes, a last line without its newline too, once the store wrote them."""
        if not self._ended:
            self._ended = True
            self._queue(self._partial)
            self._partial = b""
            self._wake.set()
        if self._sending is not None:
            await self._sending
        if self._failure is not None:
            raise self._failure

    def _queue(self, data: bytes) -> None:
        """Holds bytes for the store, or drops and counts their lines when it holds no room."""
        if not data:
            return
        if self._failure is not None or self._queued_bytes + len(data) > _LINES_HELD:
            self._dropped += _lines_in(data)
            return
        self._queued.append(data)
        self._queued_bytes += len(data)
        self._wake.set()

    async def _send(self, stream: Stream, piece: int) -> None:
        """Sends what is held as it comes, in pieces within the stream's credit, then the end."""
        try:
            while True:
                if not self._queued:
                    if self._ended:
                        break
                    self._wake.clear()
                    await self._wake.wait()
                    continue
                data = self._queued.pop(0)
                self._queued_bytes -= len(data)
                for at in range(0, len(data), piece):
                    await stream.send(RecordsPiece.encode(bytes=data[at : at + piece]), False)
            await stream.send(b"", True)
            while True:
                event = await stream.next()
                if event.end:
                    self._dropped += RecordsDropped.decode(event.body).get("dropped", 0)
                    return
        except Exception as err:
            self._failure = err
            for data in self._queued:
                self._dropped += _lines_in(data)
            self._queued.clear()
            self._queued_bytes = 0


def _lines_in(data: bytes) -> int:
    """The lines bytes hold: one a newline ends, and one more past the last."""
    return data.count(b"\n") + (0 if data.endswith(b"\n") else 1)
