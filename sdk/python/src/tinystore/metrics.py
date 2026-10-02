"""Series of samples in metrics.db, as Go's metrics keeps them.

A sample is kept bit for bit, a range read exactly, an aggregate computed
exactly and rounded once. Instruments live in the SDK and are ingested every
15 seconds, and once more as the store closes.
"""

from __future__ import annotations

import asyncio
import contextlib
import time
from array import array
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Literal

from ._connection import Connection, Link, download
from ._time import Duration, date_of, ms, unix_ms
from ._wire.messages import (
    METHODS,
    MetricsBatch,
    MetricsBuckets,
    MetricsDropped,
    MetricsLabels,
    MetricsRange,
    MetricsSeries,
)
from .errors import InvalidError

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Mapping, Sequence
    from datetime import datetime

type Kind = Literal["gauge", "counter"]
type Op = Literal["count", "sum", "min", "max", "increase"]
_OPEN_END = (1 << 63) - 1
_FLUSH_EVERY = 15.0


@dataclass(frozen=True, slots=True)
class Series:
    name: str
    kind: str
    labels: dict[str, str]
    times: list[int]
    """unix milliseconds, in time order"""
    values: array[float]
    """their bits as they were ingested: -0 and a NaN's payload survive"""


@dataclass(frozen=True, slots=True)
class Bucket:
    from_: datetime | None
    to: datetime | None
    count: int
    resets: int
    value: float
    overflow: bool
    partial: bool
    """retention cut the bucket, which counted only its samples from the cutoff on"""


@dataclass(frozen=True, slots=True)
class Aggregate:
    name: str
    kind: str
    labels: dict[str, str]
    buckets: list[Bucket]


_WIRE_NAME = "__name__"
"""The label the wire carries a series' name as, the store's own spelling."""


def _wire_labels(name: str | None, labels: Mapping[str, str] | None) -> dict[str, str]:
    """A series' name and labels as the wire spells them, its name among its labels."""
    spelled: dict[str, str] = {}
    for key, value in (labels or {}).items():
        if key.startswith("__"):
            raise InvalidError(f"the label {key}: a name beginning with __ is the store's own")
        spelled[key] = value
    if name is not None:
        spelled[_WIRE_NAME] = name
    return spelled


def _named(spelled: Mapping[str, str]) -> tuple[str, dict[str, str]]:
    """A series as the wire spelled it: its name apart from its labels."""
    labels = dict(spelled)
    return labels.pop(_WIRE_NAME, ""), labels


@dataclass(frozen=True, slots=True)
class Condition:
    """What a label's value must be beyond equality, made by one_of, none_of or prefix.

    An object of the SDK's own, so that a stored value is never read as a query.
    """

    kind: Literal["one_of", "none_of", "prefix"]
    values: tuple[str, ...]


def one_of(*values: str) -> Condition:
    """The label is one of the values."""
    return Condition("one_of", values)


def none_of(*values: str) -> Condition:
    """The label is none of the values, or the series has none."""
    return Condition("none_of", values)


def prefix(start: str) -> Condition:
    """The label's value begins with start."""
    return Condition("prefix", (start,))


def _conditions(
    match: Mapping[str, str] | None, where: Mapping[str, Condition | str] | None
) -> tuple[dict[str, str], list[dict[str, Any]]]:
    """A range's equality, plain values of where included, and its conditions in the order of their labels."""
    equal = dict(match or {})
    conditions: list[dict[str, Any]] = []
    for label, condition in sorted((where or {}).items()):
        if label in equal:
            raise InvalidError(f"label {label} is matched and has a condition")
        if isinstance(condition, str):
            equal[label] = condition
        elif isinstance(condition, Condition):  # pyright: ignore[reportUnnecessaryIsInstance]
            conditions.append({"label": label, "kind": condition.kind, "values": list(condition.values)})
        else:
            raise InvalidError(f"label {label}: a condition is made by one_of, none_of or prefix")
    return equal, conditions


def _range(
    name: str | None,
    match: Mapping[str, str] | None,
    where: Mapping[str, Condition | str] | None,
    since: Duration | None,
    from_: datetime | int | None,
    to: datetime | int | None,
    limits: Mapping[str, int] | None,
    **extra: Any,
) -> bytes:
    equal, conditions = _conditions(match, where)
    if name is None and not equal and all(c["kind"] == "none_of" for c in conditions):
        raise InvalidError("a range names a series, a label to match, or a one_of or a prefix")
    if since is not None and from_ is not None:
        raise InvalidError("a range starts since a span before now or from a time, not both")

    def millis(t: datetime | int) -> int:
        return t if isinstance(t, int) else unix_ms(t)

    start = time.time_ns() // 1_000_000 - ms(since) if since is not None else 0 if from_ is None else millis(from_)
    return MetricsRange.encode(
        matchers=_wire_labels(name, equal),
        where=conditions or None,
        from_=start,
        to=_OPEN_END if to is None else millis(to),
        **{f"limit_{k}": v for k, v in (limits or {}).items()},
        **extra,
    )


class Metrics:
    def __init__(self, link: Link) -> None:
        self._link = link
        self._instruments: dict[str, _Instrument] = {}
        self._task: asyncio.Task[None] | None = None

    async def ingest(self, *series: Mapping[str, Any]) -> None:
        """Stores series and their samples, all or none; a refused series is named by its labels.

        A series is a name, a kind, labels when it has any, and samples as
        (time, value) pairs, or times in unix milliseconds and values::

            await store.metrics.ingest({"name": "cpu", "kind": "gauge", "labels": {"host": "web-1"},
                                        "samples": [(datetime.now(UTC), 0.42)]})
        """
        batch: list[dict[str, Any]] = []
        for s in series:
            if "samples" in s:
                pairs: Sequence[tuple[datetime | int, float]] = s["samples"]
                times = array("q", (t if isinstance(t, int) else unix_ms(t) for t, _ in pairs))
                values = array("d", (v for _, v in pairs))
            else:
                times, values = array("q", s["times"]), array("d", s["values"])
            labels = _wire_labels(s["name"], s.get("labels"))
            batch.append({"labels": labels, "kind": s["kind"], "times": times, "values": values})
        await self._send(batch)

    async def _send(self, batch: list[dict[str, Any]]) -> None:
        async def attempt(connection: Connection) -> None:
            await connection.session.call(METHODS["metrics.ingest"], MetricsBatch.encode(series=batch))

        await self._link.run("write", attempt)

    async def read(
        self,
        *,
        name: str | None = None,
        match: Mapping[str, str] | None = None,
        where: Mapping[str, Condition | str] | None = None,
        since: Duration | None = None,
        from_: datetime | int | None = None,
        to: datetime | int | None = None,
        limits: Mapping[str, int] | None = None,
    ) -> list[Series]:
        """Every sample of the series a range matches, exactly.

        The series of a name, of labels, or of both, each matched exactly; over
        the last ``since``, or from ``from_`` to ``to``, in datetimes or unix
        milliseconds, ``to`` open when absent::

            await store.metrics.read(name="cpu", since="1h")
            await store.metrics.read(match={"host": "web-1"}, from_=start, to=end)
        """
        body = _range(name, match, where, since, from_, to, limits)

        async def attempt(connection: Connection) -> Any:
            return await download(connection, METHODS["metrics.read"], body)

        joined: list[Series] = []
        for item in (await self._link.run("read", attempt)).items:
            s = MetricsSeries.decode(item)
            (series_name, labels), kind = _named(s.get("labels", {})), s.get("kind", "gauge")
            if joined and (joined[-1].name, joined[-1].labels) == (series_name, labels):
                joined[-1].times.extend(s.get("times", []))
                joined[-1].values.extend(s.get("values", array("d")))
            else:
                joined.append(Series(series_name, kind, labels, list(s.get("times", [])), s.get("values", array("d"))))
        return joined

    async def aggregate(
        self,
        *,
        width: Duration,
        op: Op,
        name: str | None = None,
        match: Mapping[str, str] | None = None,
        where: Mapping[str, Condition | str] | None = None,
        since: Duration | None = None,
        from_: datetime | int | None = None,
        to: datetime | int | None = None,
        limits: Mapping[str, int] | None = None,
    ) -> list[Aggregate]:
        """Buckets of a width from the range's start, each computed exactly: an increase counts resets."""
        body = _range(name, match, where, since, from_, to, limits, width=ms(width), op=op)

        async def attempt(connection: Connection) -> Any:
            return await download(connection, METHODS["metrics.aggregate"], body)

        joined: list[Aggregate] = []
        for item in (await self._link.run("read", attempt)).items:
            b = MetricsBuckets.decode(item)
            flags = b.get("flags", b"")
            buckets = [
                Bucket(
                    date_of(b["from_"][i]),
                    date_of(b["to"][i]),
                    b["count"][i],
                    b["resets"][i],
                    v,
                    bool(flags[i] & 1),
                    bool(flags[i] & 2),
                )
                for i, v in enumerate(b.get("values", []))
            ]
            series_name, labels = _named(b.get("labels", {}))
            if joined and (joined[-1].name, joined[-1].labels) == (series_name, labels):
                joined[-1].buckets.extend(buckets)
            else:
                joined.append(Aggregate(series_name, b.get("kind", "gauge"), labels, buckets))
        return joined

    async def drop(self, name: str, labels: Mapping[str, str] | None = None) -> tuple[bool, int]:
        """Removes one series and everything it holds: whether it was there, and the unreadable groups removed."""
        spelled = _wire_labels(name, labels)

        async def attempt(connection: Connection) -> bytes:
            return await connection.session.call(METHODS["metrics.drop"], MetricsLabels.encode(labels=spelled))

        dropped = MetricsDropped.decode(await self._link.run("write", attempt))
        return bool(dropped.get("found")), dropped.get("unreadable_groups", 0)

    def counter(self, name: str) -> Counter:
        """A counter: its total since this process started, ingested every flush; a restart is a reset."""
        return Counter(self._instrument(name, "counter"), {})

    def gauge(self, name: str) -> Gauge:
        return Gauge(self._instrument(name, "gauge"), {})

    def gauge_func(self, name: str, read: Callable[[], float | Awaitable[float]]) -> None:
        """A gauge read by a function at each flush; one that raises skips that sample."""
        self._instrument(name, "gauge").read = read

    def _instrument(self, name: str, kind: Kind) -> _Instrument:
        instrument = self._instruments.get(name)
        if instrument is None:
            instrument = self._instruments[name] = _Instrument(name, kind)
            if self._task is None:
                self._task = asyncio.get_running_loop().create_task(self._flushing())
        elif instrument.kind != kind:
            raise InvalidError(f"{name} is a {instrument.kind}, not a {kind}")
        return instrument

    async def _flushing(self) -> None:
        while True:
            await asyncio.sleep(_FLUSH_EVERY)
            with contextlib.suppress(Exception):
                await self.flush()

    async def flush(self) -> None:
        """Ingests every instrument's value now, as the timer does every 15 s."""
        now = time.time_ns() // 1_000_000
        batch: list[dict[str, Any]] = []
        for instrument in self._instruments.values():
            if instrument.read is not None:
                try:
                    got = instrument.read()
                    value = await got if asyncio.iscoroutine(got) or isinstance(got, asyncio.Future) else got
                except Exception:
                    continue
                instrument.series[()] = ({}, float(value))  # type: ignore[arg-type]
            for labels, value in instrument.series.values():
                batch.append(
                    {
                        "labels": {**labels, _WIRE_NAME: instrument.name},
                        "kind": instrument.kind,
                        "times": array("q", [now]),
                        "values": array("d", [value]),
                    }
                )
        if batch:
            await self._send(batch)

    async def stop(self) -> None:
        if self._task is not None:
            self._task.cancel()
            self._task = None
            with contextlib.suppress(Exception):
                await self.flush()


class _Instrument:
    def __init__(self, name: str, kind: Kind) -> None:
        self.name, self.kind = name, kind
        self.series: dict[tuple[tuple[str, str], ...], tuple[dict[str, str], float]] = {}
        self.read: Callable[[], float | Awaitable[float]] | None = None

    def add(self, labels: dict[str, str], n: float, replace: bool = False) -> None:
        key = tuple(sorted(labels.items()))
        old = self.series.get(key, (labels, 0.0))[1]
        self.series[key] = (labels, n if replace else old + n)


class Counter:
    def __init__(self, instrument: _Instrument, labels: dict[str, str]) -> None:
        self._instrument, self._labels = instrument, labels

    def labels(self, **labels: str) -> Counter:
        """The counter of these labels besides its own: the same labels in any order are one series."""
        return Counter(self._instrument, {**self._labels, **labels})

    def inc(self, n: float = 1) -> None:
        if not n >= 0:
            raise InvalidError(f"a counter adds {n}; it only grows")
        self._instrument.add(self._labels, n)


class Gauge:
    def __init__(self, instrument: _Instrument, labels: dict[str, str]) -> None:
        self._instrument, self._labels = instrument, labels

    def labels(self, **labels: str) -> Gauge:
        return Gauge(self._instrument, {**self._labels, **labels})

    def set(self, value: float) -> None:
        self._instrument.add(self._labels, value, replace=True)

    def inc(self, n: float = 1) -> None:
        self._instrument.add(self._labels, n)

    def dec(self, n: float = 1) -> None:
        self._instrument.add(self._labels, -n)
