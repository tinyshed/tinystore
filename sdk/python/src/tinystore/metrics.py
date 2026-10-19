"""Numbers that move over time, as docs/guides/metrics/README.md has them.

Series by their name and labels, written whole by ingest and read back by
range, in buckets and as their newest samples; what a store holds of them;
and a program's instruments, whose totals the store keeps. A series' samples
travel as two columns, an int64 time and a float64 value a sample, so that
every value's bits come back as they were written::

    requests = store.metrics.counter("http_requests_total", help="Requests served.")
    requests.labels(route="/notes", status=200).inc()
    traffic = await store.metrics.aggregate(name="http_requests_total", since="24h", every="1h", op="increase")
"""

from __future__ import annotations

import struct
import sys
from array import array
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from datetime import datetime
from typing import TYPE_CHECKING, Any, Literal, TypedDict, Unpack, cast

from ._connection import download
from ._time import moment as _moment
from ._time import ms, spelled_keep, unix_ms
from ._wire.protocol import (
    Empty,
    MetricsAggregateAsked,
    MetricsAggregates,
    MetricsDamages,
    MetricsDeleted,
    MetricsDescription,
    MetricsIngest,
    MetricsLatest,
    MetricsNameAsked,
    MetricsNamePage,
    MetricsPageAsked,
    MetricsPlan,
    MetricsQuery,
    MetricsRead,
    MetricsSeriesNamed,
    MetricsTextPage,
    methods,
)
from .errors import InvalidError
from .instruments import (
    Counter,
    Gauge,
    Histogram,
    Instruments,
    LabelValue,
    Timer,
    Timing,
    exponential_buckets,
    label_text,
    labels_of,
)

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

    from ._connection import Link
    from ._time import Duration

__all__ = [
    "Aggregate",
    "AggregateBucket",
    "AggregateQuery",
    "Counter",
    "Description",
    "Exceeds",
    "Gauge",
    "Histogram",
    "LabelPage",
    "Metrics",
    "MetricsDamage",
    "NameInfo",
    "NamePage",
    "Plan",
    "Point",
    "Series",
    "SeriesQuery",
    "Timer",
    "Timing",
    "ValuePage",
    "exponential_buckets",
]

SeriesKind = Literal["gauge", "counter", "histogram"]
"""What a series' samples are: a histogram is the _bucket series of one written elsewhere."""

Condition = LabelValue | Sequence[LabelValue] | Mapping[str, Any]
"""What a label's value must be: a value, one of a list, {"not": …} (or the label absent), or {"prefix": "api-"}."""

Op = Literal[
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "increase",
    "rate",
    "delta",
    "first",
    "last",
    "p50",
    "p75",
    "p90",
    "p95",
    "p99",
    "p99.9",
]
"""What a bucket answers: count … last, or a percentile, p and a rank; rate is a second's increase."""

Time = datetime | int
"""A sample's time: an aware datetime, or unix milliseconds."""


class SeriesQuery(TypedDict, total=False):
    """The series a call finds and its range; a condition given as None is InvalidError, never left out."""

    name: str
    """the series of this name; a timer's name, the series it writes"""
    where: Mapping[str, Condition]
    since: Duration
    """the span before now the range covers; or from_ and to"""
    from_: datetime
    """the range's start, included"""
    to: datetime
    """the range's end, excluded; now, a sample taken now in the range, unless given"""


class AggregateQuery(SeriesQuery, total=False):
    """An aggregate: a query cut into buckets of every, each one exact value of op, by groups."""

    every: Duration
    """the buckets' span, counted from 1970 in UTC; the range is one bucket without it"""
    op: Op | str
    by: Sequence[str]
    """the labels a group keeps; without by and without, every series of a name is one group"""
    without: Sequence[str]
    """the labels a group leaves out, keeping every other; [] keeps every series apart"""
    estimate: bool
    """a timer's percentile also gives a point between its bounds, interpolated: an estimate"""


@dataclass(slots=True)
class Series:
    """A series: what ingest writes, and what a read gives back.

    Ingest takes its samples as (time, value) pairs, or as times and values
    columns, a time an aware datetime or unix milliseconds. A read gives
    ``times``, unix milliseconds in an ``array('q')``, and ``values``, an
    ``array('d')`` whose bytes keep every value's bits: -0 and a NaN's
    payload survive.
    """

    name: str
    kind: SeriesKind
    labels: Mapping[str, LabelValue] = field(default_factory=dict[str, LabelValue])
    samples: Sequence[tuple[Time, float]] | None = None
    times: Sequence[Time] | None = None
    values: Sequence[float] | None = None
    keep: Duration | None = None
    """how long the name's samples stay: its own, kept in the file; unless given, what it keeps already"""


@dataclass(frozen=True, slots=True)
class AggregateBucket:
    """A bucket of an aggregate.

    A bucket of count … last has its value; a percentile's is a gauge's sample
    at the rank, or a timer's bucket of its spread that holds it, from lower
    to upper, and with estimate asked a point between them.
    """

    at: datetime
    """where it begins"""
    samples: int
    """how many samples it comes from; a timer's, how many durations"""
    resets: int
    partial: bool
    """its value comes from less than its span: the range, the clock or retention cut it"""
    gap: bool
    """its first step began before the bucket before it: a whole bucket between had no samples"""
    value: float | None = None
    lower: float | None = None
    upper: float | None = None
    """infinity past the last bound"""
    estimate: float | None = None


@dataclass(frozen=True, slots=True)
class Aggregate:
    name: str
    labels: dict[str, str]
    """the labels by kept or without left"""
    buckets: list[AggregateBucket]
    """a bucket without samples is left out"""


@dataclass(frozen=True, slots=True)
class Point:
    """A series' newest sample in the range."""

    name: str
    labels: dict[str, str]
    at: datetime
    value: float


@dataclass(frozen=True, slots=True)
class Exceeds:
    limit: str
    """series, bytes read or samples decoded"""
    wanted: int
    bound: int


@dataclass(frozen=True, slots=True)
class Plan:
    """What a read or an aggregate would spend of each bound, and the bound it would stop at."""

    series: int
    bytes: int
    decoded: int
    exceeds: Exceeds | None


@dataclass(frozen=True, slots=True)
class NameInfo:
    """A name the store holds samples of."""

    name: str
    kind: SeriesKind
    series_count: int
    unit: str
    help: str
    keep: str
    retained_from: datetime
    """its oldest sample, or where its keep begins while it holds samples from before"""
    newest: datetime


@dataclass(frozen=True, slots=True)
class NamePage:
    names: list[NameInfo]
    next: str | None
    """where the next page starts; none on the last"""


@dataclass(frozen=True, slots=True)
class LabelPage:
    labels: list[str]
    next: str | None


@dataclass(frozen=True, slots=True)
class ValuePage:
    values: list[str]
    next: str | None


@dataclass(frozen=True, slots=True)
class Description:
    """A name's unit and help; both empty when nothing described it."""

    unit: str
    help: str


@dataclass(frozen=True, slots=True)
class MetricsDamage:
    """A series whose bytes no longer read, which delete removes."""

    name: str
    labels: dict[str, str]
    error: str


class Metrics:
    """A store's metrics: series by name and labels, their reads and aggregates, and the program's instruments."""

    def __init__(self, link: Link, instruments: Instruments) -> None:
        self._link = link
        self._instruments = instruments

    async def ingest(self, series: Series | Sequence[Series]) -> None:
        """Writes series and their samples, every one or none: a refused series names itself."""
        given = [series] if isinstance(series, Series) else list(series)
        body = MetricsIngest.encode(series=[_new_series(each) for each in given])
        await self._link.run("write", lambda c: c.session.call(methods["metrics.ingest"], body))

    async def read(self, **query: Unpack[SeriesQuery]) -> list[Series]:
        """The samples of each series the query finds, in its range: whole, or a LimitError naming its bound."""
        parts = await self._download("metrics.read", MetricsQuery.encode(_query_of(query)))
        series: list[Series] = []
        more = False
        for piece in (each for part in parts for each in MetricsRead.decode(part).get("series", [])):
            times, values = _times_of(piece.get("times", b"")), _values_of(piece.get("values", b""))
            if more and series:
                cast("array[int]", series[-1].times).extend(times)
                cast("array[float]", series[-1].values).extend(values)
            else:
                kind = piece.get("kind", "gauge")
                series.append(Series(piece.get("name", ""), kind, dict(piece.get("labels", {})), None, times, values))
            more = piece.get("more", False)
        return series

    async def latest(self, **query: Unpack[SeriesQuery]) -> list[Point]:
        """The newest sample of each series the query finds, when it has one in the range."""
        parts = await self._download("metrics.latest", MetricsQuery.encode(_query_of(query)))
        return [
            Point(
                each.get("name", ""), dict(each.get("labels", {})), _moment(each.get("at", 0)), each.get("value", 0.0)
            )
            for part in parts
            for each in MetricsLatest.decode(part).get("series", [])
        ]

    async def aggregate(self, **query: Unpack[AggregateQuery]) -> list[Aggregate]:
        """The buckets of each group of the series the query finds, each one exact value of op, rounded once."""
        if "op" not in query:
            raise InvalidError(
                "an aggregate takes an op: count, sum, avg, min, max, increase, rate, delta, first, last, p95"
            )
        parts = await self._download("metrics.aggregate", MetricsAggregateAsked.encode(_aggregate_of(query)))
        groups: list[Aggregate] = []
        more = False
        for piece in (each for part in parts for each in MetricsAggregates.decode(part).get("groups", [])):
            buckets = [_bucket_of(bucket) for bucket in piece.get("buckets", [])]
            if more and groups:
                groups[-1].buckets.extend(buckets)
            else:
                groups.append(Aggregate(piece.get("name", ""), dict(piece.get("labels", {})), buckets))
            more = piece.get("more", False)
        return groups

    async def explain(self, **query: Unpack[AggregateQuery]) -> Plan:
        """What a read, or an aggregate when given its op, would spend of each bound; it decodes no sample."""
        asked = _aggregate_of(query) if "op" in query else {"query": _query_of(query)}
        plan = MetricsPlan.decode(await self._read("metrics.explain", MetricsAggregateAsked.encode(asked)))
        exceeds = plan.get("exceeds")
        return Plan(
            plan.get("series", 0),
            plan.get("bytes", 0),
            plan.get("decoded", 0),
            None
            if exceeds is None
            else Exceeds(exceeds.get("limit", ""), exceeds.get("wanted", 0), exceeds.get("bound", 0)),
        )

    async def names(self, *, prefix: str | None = None, limit: int | None = None, after: str | None = None) -> NamePage:
        """A page of the names the store holds samples of, in the byte order of their text."""
        asked = MetricsPageAsked.encode(prefix=prefix, limit=limit, after=after)
        page = MetricsNamePage.decode(await self._read("metrics.names", asked))
        names = [
            NameInfo(
                each.get("name", ""),
                each.get("kind", "gauge"),
                each.get("series_count", 0),
                each.get("unit", ""),
                each.get("help", ""),
                spelled_keep(each.get("keep", 0)),
                _moment(each.get("retained_from", 0)),
                _moment(each.get("newest", 0)),
            )
            for each in page.get("names", [])
        ]
        return NamePage(names, page.get("next"))

    async def label_names(
        self, name: str, *, prefix: str | None = None, limit: int | None = None, after: str | None = None
    ) -> LabelPage:
        """A page of a name's label names, but the store's own."""
        asked = MetricsPageAsked.encode(name=name, prefix=prefix, limit=limit, after=after)
        page = MetricsTextPage.decode(await self._read("metrics.labelNames", asked))
        return LabelPage(list(page.get("texts", [])), page.get("next"))

    async def label_values(
        self, name: str, label: str, *, prefix: str | None = None, limit: int | None = None, after: str | None = None
    ) -> ValuePage:
        """A page of the values a label of a name's series holds."""
        asked = MetricsPageAsked.encode(name=name, label=label, prefix=prefix, limit=limit, after=after)
        page = MetricsTextPage.decode(await self._read("metrics.labelValues", asked))
        return ValuePage(list(page.get("texts", [])), page.get("next"))

    async def describe(self, name: str, *, unit: str = "", help: str = "") -> None:
        """Sets a name's unit and help in place of what it had: what is not given is removed.

        They belong to the name, so a delete of its series leaves them.
        """
        body = MetricsDescription.encode(name=name, unit=unit, help=help)
        await self._link.run("write", lambda c: c.session.call(methods["metrics.describe"], body))

    async def description(self, name: str) -> Description:
        """A name's unit and help; both empty when nothing described it."""
        answer = MetricsDescription.decode(await self._read("metrics.description", MetricsNameAsked.encode(name=name)))
        return Description(answer.get("unit", ""), answer.get("help", ""))

    async def delete(self, *, name: str, labels: Mapping[str, LabelValue]) -> bool:
        """Removes the one series of exactly this name and these labels, whether its samples still read or not.

        True when the store held it; ``labels={}`` is the series without labels.
        """
        texts = {label: label_text(label, value) for label, value in labels.items()}
        body = MetricsSeriesNamed.encode(name=name, labels=texts)
        answer = await self._link.run("write", lambda c: c.session.call(methods["metrics.delete"], body))
        return MetricsDeleted.decode(answer).get("found", False)

    async def damaged(self) -> list[MetricsDamage]:
        """The series whose bytes no longer read, 1000 at most, each with what broke; deleting them shows the next."""
        answer = MetricsDamages.decode(await self._read("metrics.damaged", Empty.encode()))
        return [
            MetricsDamage(each.get("name", ""), dict(each.get("labels", {})), each.get("error", ""))
            for each in answer.get("damages", [])
        ]

    async def flush(self) -> None:
        """Sends what every instrument changed and has the store write its totals now, as it does every 15 seconds."""
        if await self._instruments.flush():
            return
        await self._link.run("write", lambda c: c.session.call(methods["metrics.flush"], Empty.encode()))

    def counter(
        self, name: str, *, unit: str | None = None, help: str | None = None, keep: Duration | None = None
    ) -> Counter:
        """A total that only goes up, which the store keeps: processes counting into one series count into one total."""
        declared = self._instruments.declare(name, "counter", unit=unit, help_=help, keep=keep)
        return Counter(self._instruments, declared, {})

    def gauge(
        self, name: str, *, unit: str | None = None, help: str | None = None, keep: Duration | None = None
    ) -> Gauge:
        """A value that goes up and down: a queue's length, a temperature."""
        declared = self._instruments.declare(name, "gauge", unit=unit, help_=help, keep=keep)
        return Gauge(self._instruments, declared, {})

    def gauge_func(
        self,
        name: str,
        read: Callable[[], float | Awaitable[float]],
        *,
        labels: Mapping[str, LabelValue] | None = None,
        unit: str | None = None,
        help: str | None = None,
        keep: Duration | None = None,
    ) -> None:
        """A gauge whose value read gives every 15 seconds, of the set of labels given.

        Another set of the name takes a function of its own. It may be async,
        is never called again while a call of it runs, and an error or an
        answer that is not a number writes nothing that time, said once while
        it repeats::

            store.metrics.gauge_func("queue_depth", emails.depth, labels={"queue": "emails"})
        """
        self._instruments.reader(name, labels_of(labels or {}, False), read, unit=unit, help_=help, keep=keep)

    def timer(
        self,
        name: str,
        *,
        buckets: Sequence[float] | Literal["exponential"] | None = None,
        help: str | None = None,
        keep: Duration | None = None,
    ) -> Timer:
        """How many things took how long: their count, sum and longest in milliseconds, and their spread."""
        declared = self._instruments.declare(name, "timer", help_=help, keep=keep, buckets=buckets)
        return Timer(self._instruments, declared, {})

    def histogram(
        self,
        name: str,
        *,
        buckets: Sequence[float] | Literal["exponential"] | None = None,
        unit: str | None = None,
        help: str | None = None,
        keep: Duration | None = None,
    ) -> Histogram:
        """A timer of numbers that are not durations: a response's size in bytes."""
        declared = self._instruments.declare(name, "histogram", unit=unit, help_=help, keep=keep, buckets=buckets)
        return Histogram(self._instruments, declared, {})

    async def _read(self, method: str, body: bytes) -> bytes:
        return await self._link.run("read", lambda c: c.session.call(methods[method], body))

    async def _download(self, method: str, body: bytes) -> list[bytes]:
        got = await self._link.run("read", lambda c: download(c, methods[method], body))
        return [got.trailer] if not got.items else [*got.items, got.trailer]


def _query_of(query: Mapping[str, Any]) -> dict[str, Any]:
    """A query as the wire carries it; a condition given as None is refused, never left out."""
    if "since" in query and "from_" in query:
        raise InvalidError("a query starts since a span before now or from a time, not both")
    since, from_, to = query.get("since"), query.get("from_"), query.get("to")
    where = cast("Mapping[str, Condition | None]", query.get("where") or {})
    return {
        "name": query.get("name"),
        "where": {label: _condition_of(label, condition) for label, condition in where.items()},
        "since": None if since is None else ms(since, "a query's since"),
        "from": None if from_ is None else unix_ms(from_, "a query's from_"),
        "to": None if to is None else unix_ms(to, "a query's to"),
    }


def _condition_of(label: str, condition: Condition | None) -> dict[str, Any]:
    if condition is None:
        raise InvalidError(f"where {label}: a condition given as None")
    if isinstance(condition, str | int | float | bool):
        return {"values": [label_text(label, condition)]}
    if isinstance(condition, Mapping):
        if "not" in condition:
            refused = condition["not"]
            values = cast("Sequence[LabelValue]", refused if isinstance(refused, list | tuple) else [refused])
            return {"values": [label_text(label, value) for value in values], "not": True}
        if "prefix" in condition:
            return {"prefix": str(condition["prefix"])}
        raise InvalidError(f'where {label}: a value, a list, {{"not": …}} or {{"prefix": …}}')
    return {"values": [label_text(label, value) for value in condition]}


def _aggregate_of(query: Mapping[str, Any]) -> dict[str, Any]:
    every, by, without = query.get("every"), query.get("by"), query.get("without")
    return {
        "query": _query_of(query),
        "every": None if every is None else ms(every, "an aggregate's every"),
        "op": query.get("op"),
        "by": None if by is None else list(by),
        "without": None if without is None else list(without),
        "estimate": query.get("estimate", False),
    }


def _bucket_of(bucket: dict[str, Any]) -> AggregateBucket:
    lower = bucket.get("lower")
    return AggregateBucket(
        at=_moment(bucket.get("at", 0)),
        samples=bucket.get("samples", 0),
        resets=bucket.get("resets", 0),
        partial=bucket.get("partial", False),
        gap=bucket.get("gap", False),
        value=bucket.get("value"),
        lower=lower,
        upper=None if lower is None else bucket.get("upper", float("inf")),
        estimate=bucket.get("estimate"),
    )


def _new_series(given: Series) -> dict[str, Any]:
    """A series to write, its samples as the two columns the wire carries."""
    labels = {label: label_text(label, value) for label, value in given.labels.items()}
    times, values = _columns_of(given)
    stamps = [_stamp(given.name, at) for at in times]
    return {
        "name": given.name,
        "kind": given.kind,
        "labels": labels,
        "times": struct.pack(f"<{len(stamps)}q", *stamps),
        "values": _value_bytes(values),
        "keep": None if given.keep is None else ms(given.keep, f"metrics {given.name}: keep"),
    }


def _columns_of(given: Series) -> tuple[Sequence[Time], Sequence[float]]:
    if given.samples is not None:
        if given.times is not None or given.values is not None:
            raise InvalidError(f"metrics {given.name}: samples, or times and values, not both")
        return [at for at, _ in given.samples], [value for _, value in given.samples]
    if given.times is None or given.values is None or len(given.times) != len(given.values):
        raise InvalidError(f"metrics {given.name}: samples, or times and values as long as each other")
    return given.times, given.values


def _stamp(name: str, at: Time) -> int:
    if isinstance(at, datetime):
        return unix_ms(at, f"metrics {name}: a sample's time")
    if isinstance(at, bool) or not isinstance(at, int):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"metrics {name}: a sample's time {at!r}, where an aware datetime or unix milliseconds is")
    return at


def _value_bytes(values: Sequence[float]) -> bytes:
    """The values' bytes as they are, a NaN's payload with them: an array('d') is copied, a list packed."""
    if isinstance(values, array) and values.typecode == "d":
        if sys.byteorder == "little":
            return values.tobytes()
        swapped = array("d", values)
        swapped.byteswap()
        return swapped.tobytes()
    return struct.pack(f"<{len(values)}d", *values)


def _times_of(data: bytes) -> array[int]:
    """A column of int64 times, unix milliseconds."""
    times = array("q")
    times.frombytes(data)
    if sys.byteorder != "little":
        times.byteswap()
    return times


def _values_of(data: bytes) -> array[float]:
    """A column of float64 values, its bytes taken as they came, so that each value's bits stay."""
    values = array("d")
    values.frombytes(data)
    if sys.byteorder != "little":
        values.byteswap()
    return values
