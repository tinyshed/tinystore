"""Series of samples in metrics.db, as Go's metrics keeps them.

A sample is kept bit for bit, a range read exactly, an aggregate computed
exactly and rounded once. Instruments live in the SDK and are ingested every
15 seconds, and once more as the store closes.
"""

from __future__ import annotations

import asyncio
import math
import time
from array import array
from dataclasses import dataclass
from datetime import timedelta
from typing import TYPE_CHECKING, Any, Literal, Self

from ._background import FailureLog, say_own
from ._connection import Connection, Link, download
from ._time import Duration, date_of, ms, unix_ms
from ._wire.messages import (
    METHODS,
    MetricsBatch,
    MetricsBuckets,
    MetricsDropped,
    MetricsLabels,
    MetricsPlan,
    MetricsRange,
    MetricsSeries,
)
from .errors import InvalidError, LimitError, error_of

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Iterable, Mapping, Sequence
    from datetime import datetime
    from types import TracebackType

type Kind = Literal["gauge", "counter"]
type Op = Literal["count", "sum", "min", "max", "avg", "increase", "rate", "delta"]
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
class Plan:
    """What a read or an aggregate would spend, each use beside its limit; stops is the limit it would reach."""

    series: int
    blocks: int
    summarized: int
    bytes: int
    decoded: int
    limits: dict[str, int]
    stops: LimitError | None


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
        self._timers: dict[str, _Timer] = {}
        self._task: asyncio.Task[None] | None = None
        self._failure_log = FailureLog("metrics instruments")
        self.failures = 0
        """instruments' flushes that failed"""
        self.last_failure: Exception | None = None

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
        by: Iterable[str] | None = None,
        without: Iterable[str] | None = None,
    ) -> list[Aggregate]:
        """Buckets of a width from the range's start, each computed exactly and rounded once.

        avg is the mean of every sample, rate a counter's increase a second,
        delta a gauge's last sample less its first. by groups the series by
        those labels, without by every label but those, each group one
        result; ``by=[]`` joins every series of a name::

            await store.metrics.aggregate(name="http_requests_total", since="1d", width="1h", op="rate", by=["route"])
        """
        if by is not None and without is not None:
            raise InvalidError("an aggregate groups by labels or without them, not both")
        grouping = {"by": None if by is None else list(by), "without": None if without is None else list(without)}
        body = _range(name, match, where, since, from_, to, limits, width=ms(width), op=op, **grouping)

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

    async def explain(
        self,
        *,
        name: str | None = None,
        match: Mapping[str, str] | None = None,
        where: Mapping[str, Condition | str] | None = None,
        since: Duration | None = None,
        from_: datetime | int | None = None,
        to: datetime | int | None = None,
        limits: Mapping[str, int] | None = None,
        width: Duration | None = None,
        op: Op | None = None,
        by: Iterable[str] | None = None,
        without: Iterable[str] | None = None,
    ) -> Plan:
        """What read, or aggregate when op is given, would spend of its limits, without reading a sample.

        The series, the blocks, those an aggregate answers from their
        summaries, the bytes and the samples, beside the limits; stops is the
        LimitError the call would end with, None when it fits.
        """
        extra: dict[str, Any] = {}
        if op is not None:
            extra = {
                "width": ms(width or 0),
                "op": op,
                "by": None if by is None else list(by),
                "without": None if without is None else list(without),
            }
        body = _range(name, match, where, since, from_, to, limits, **extra)

        async def attempt(connection: Connection) -> bytes:
            return await connection.session.call(METHODS["metrics.explain"], body)

        p = MetricsPlan.decode(await self._link.run("read", attempt))
        stops = p.get("stops")
        refused = None
        if stops is not None:
            refused = error_of(stops.get("code", "limit"), stops.get("message", ""), stops.get("what"))
        return Plan(
            series=p.get("series", 0),
            blocks=p.get("blocks", 0),
            summarized=p.get("summarized", 0),
            bytes=p.get("bytes", 0),
            decoded=p.get("decoded", 0),
            limits={k: p.get(f"limit_{k}", 0) for k in ("series", "blocks", "bytes", "decoded", "answered")},
            stops=refused if isinstance(refused, LimitError) else None,
        )

    def counter(self, name: str) -> Counter:
        """A counter: its total since this process started, ingested every flush; a restart is a reset."""
        return Counter(self._instrument(name, "counter"), {})

    def gauge(self, name: str) -> Gauge:
        return Gauge(self._instrument(name, "gauge"), {})

    def gauge_func(self, name: str, read: Callable[[], float | Awaitable[float]]) -> None:
        """A gauge read by a function at each flush; one that raises skips that sample."""
        self._instrument(name, "gauge").read = read

    def timer(self, name: str) -> Timer:
        """A timer: how many durations it measured and their sum in milliseconds, and the longest.

        Every flush ingests the first two as the counters name_count and
        name_sum, and the longest since the flush before as the gauge
        name_max, left out when it measured none. A range's mean is its sum's
        increase over its count's.
        """
        timer = self._timers.get(name)
        if timer is None:
            for suffix in _TIMER_SUFFIXES:
                writer = self._instruments.get(name + suffix)
                if writer is not None:
                    raise InvalidError(f"{name + suffix} is a {writer.kind}; the timer {name} would write it")
            timer = self._timers[name] = _Timer(name)
            self._flushing_soon()
        return Timer(timer, {})

    def _instrument(self, name: str, kind: Kind) -> _Instrument:
        instrument = self._instruments.get(name)
        if instrument is None:
            for suffix in _TIMER_SUFFIXES:
                if name.endswith(suffix) and name.removesuffix(suffix) in self._timers:
                    raise InvalidError(f"{name} is written by the timer {name.removesuffix(suffix)}")
            instrument = self._instruments[name] = _Instrument(name, kind)
            self._flushing_soon()
        elif instrument.kind != kind:
            raise InvalidError(f"{name} is a {instrument.kind}, not a {kind}")
        return instrument

    def _flushing_soon(self) -> None:
        if self._task is None:
            self._task = asyncio.get_running_loop().create_task(self._flushing())

    async def _flushing(self) -> None:
        while True:
            await asyncio.sleep(_FLUSH_EVERY)
            await self._flush_in_background()

    async def _flush_in_background(self) -> None:
        """A flush no caller awaits says its failure, as Go's background work does."""
        try:
            await self.flush()
        except Exception as err:
            self._failure_log.failed(err)
        else:
            self._failure_log.succeeded()

    async def flush(self) -> None:
        """Ingests every instrument's value now, as the timer does every 15 s."""
        now = time.time_ns() // 1_000_000
        batch: list[dict[str, Any]] = []
        for instrument in list(self._instruments.values()):
            if instrument.read is not None and not await _read_gauge(instrument, instrument.read):
                continue
            for labels, value in instrument.series.values():
                batch.append(_sample_at(now, instrument.name, labels, instrument.kind, value))
        taken = self._take_timers(now, batch)
        if not batch:
            return
        try:
            await self._send(batch)
        except BaseException as err:
            for timing, longest in taken:
                timing.longest = max(timing.longest, longest)
                timing.measured = True
            if isinstance(err, Exception):
                self.failures += 1
                self.last_failure = err
            raise

    def _take_timers(self, now: int, batch: list[dict[str, Any]]) -> list[tuple[_Timing, float]]:
        """Adds what the timers ingest to batch, and starts their longest again.

        It answers what it took, so that a flush that fails gives it back.
        """
        taken: list[tuple[_Timing, float]] = []
        for timer in self._timers.values():
            for timing in timer.series.values():
                batch.append(_sample_at(now, timer.name + "_count", timing.labels, "counter", timing.count))
                batch.append(_sample_at(now, timer.name + "_sum", timing.labels, "counter", timing.sum))
                if timing.measured:
                    batch.append(_sample_at(now, timer.name + "_max", timing.labels, "gauge", timing.longest))
                    taken.append((timing, timing.longest))
                    timing.longest, timing.measured = 0.0, False
        return taken

    async def stop(self) -> None:
        """Stops flushing every 15 s and ingests the instruments' last values, as the store does as it closes."""
        if self._task is not None:
            self._task.cancel()
            self._task = None
            await self._flush_in_background()


def _sample_at(now: int, name: str, labels: dict[str, str], kind: Kind, value: float) -> dict[str, Any]:
    return {
        "labels": {**labels, _WIRE_NAME: name},
        "kind": kind,
        "times": array("q", [now]),
        "values": array("d", [value]),
    }


class _Instrument:
    def __init__(self, name: str, kind: Kind) -> None:
        self.name = name
        self.kind: Kind = kind
        self.series: dict[tuple[tuple[str, str], ...], tuple[dict[str, str], float]] = {}
        self.read: Callable[[], float | Awaitable[float]] | None = None
        self.last_read: str | None = None
        """the error a gauge's function last raised, said once while it stays the same"""

    def add(self, labels: dict[str, str], n: float, replace: bool = False) -> None:
        key = tuple(sorted(labels.items()))
        old = self.series.get(key, (labels, 0.0))[1]
        self.series[key] = (labels, n if replace else old + n)


async def _read_gauge(instrument: _Instrument, read: Callable[[], float | Awaitable[float]]) -> bool:
    """Sets a gauge's value from its function, or says why not, once while the error stays the same."""
    try:
        got = read()
        value = await got if asyncio.iscoroutine(got) or isinstance(got, asyncio.Future) else got
    except Exception as err:
        if str(err) != instrument.last_read:
            say_own("warn", "gauge read failed", {"series": instrument.name, "error": str(err)})
            instrument.last_read = str(err)
        return False
    instrument.series[()] = ({}, float(value))  # type: ignore[arg-type]
    instrument.last_read = None
    return True


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


_TIMER_SUFFIXES = ("_count", "_sum", "_max")


@dataclass(slots=True)
class _Timing:
    labels: dict[str, str]
    count: int = 0
    sum: float = 0.0  # milliseconds
    longest: float = 0.0  # milliseconds, since the flush before
    measured: bool = False  # since the flush before


class _Timer:
    def __init__(self, name: str) -> None:
        self.name = name
        self.series: dict[tuple[tuple[str, str], ...], _Timing] = {}

    def add(self, labels: dict[str, str], millis: float) -> None:
        key = tuple(sorted(labels.items()))
        timing = self.series.get(key)
        if timing is None:
            timing = self.series[key] = _Timing(labels)
        timing.count += 1
        timing.sum += millis
        timing.longest = max(timing.longest, millis)
        timing.measured = True


class Timer:
    def __init__(self, timer: _Timer, labels: dict[str, str]) -> None:
        self._timer, self._labels = timer, labels

    def labels(self, **labels: str) -> Timer:
        """The timer of these labels besides its own: the same labels in any order are one series."""
        return Timer(self._timer, {**self._labels, **labels})

    def record(self, d: Duration) -> None:
        """Adds one duration: seconds, a timedelta, or text such as "250ms"."""
        if isinstance(d, str):
            millis: float = ms(d)
        elif isinstance(d, timedelta):
            millis = d / timedelta(milliseconds=1)
        else:
            millis = float(d) * 1000
        if not (math.isfinite(millis) and millis >= 0):
            raise InvalidError(f"a timer records {d!r}; a duration is never negative")
        self._timer.add(self._labels, millis)

    def measure(self) -> Measured:
        """Times the block of a with, whether it returns or raises; await inside it is timed too.

        with latency.labels(route="/users").measure():
            user = await users.get(user_id)
        """
        return Measured(self._timer, self._labels)


class Measured:
    """The block a timer's measure times, in a with or an async with.

    It is not a decorator: one on an async function would time making its
    coroutine, not running it.
    """

    def __init__(self, timer: _Timer, labels: dict[str, str]) -> None:
        self._timer, self._labels = timer, labels
        self._start = 0

    def __enter__(self) -> Self:
        self._start = time.perf_counter_ns()
        return self

    def __exit__(
        self, kind: type[BaseException] | None, error: BaseException | None, trace: TracebackType | None
    ) -> None:
        self._timer.add(self._labels, (time.perf_counter_ns() - self._start) / 1e6)

    async def __aenter__(self) -> Self:
        return self.__enter__()

    async def __aexit__(
        self, kind: type[BaseException] | None, error: BaseException | None, trace: TracebackType | None
    ) -> None:
        self.__exit__(kind, error, trace)
