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
    labels: dict[str, str]
    kind: str
    times: list[int]
    """unix milliseconds, in time order"""
    values: array[float]
    """their bits as they were ingested: -0 and a NaN's payload survive"""


@dataclass(frozen=True, slots=True)
class Bucket:
    start: datetime | None
    end: datetime | None
    count: int
    resets: int
    value: float
    overflow: bool
    partial: bool
    """retention cut the bucket, which counted only its samples from the cutoff on"""


@dataclass(frozen=True, slots=True)
class Aggregate:
    labels: dict[str, str]
    kind: str
    buckets: list[Bucket]


def _range(match: Mapping[str, str], start: datetime | int, end: datetime | int | None, **extra: Any) -> bytes:
    if not match:
        raise InvalidError("a range matches one label at least")

    def millis(t: datetime | int) -> int:
        return t if isinstance(t, int) else unix_ms(t)

    return MetricsRange.encode(
        matchers=dict(match),
        from_=millis(start),
        to=_OPEN_END if end is None else millis(end),
        **extra,
    )


class Metrics:
    def __init__(self, link: Link) -> None:
        self._link = link
        self._instruments: dict[str, _Instrument] = {}
        self._task: asyncio.Task[None] | None = None

    async def ingest(self, *series: Mapping[str, Any]) -> None:
        """Stores series and their samples, all or none; a refused series is named by its labels.

        A series is labels (__name__ among them), kind, and samples as
        (time, value) pairs, or times in unix milliseconds and values.
        """
        batch: list[dict[str, Any]] = []
        for s in series:
            if "samples" in s:
                pairs: Sequence[tuple[datetime | int, float]] = s["samples"]
                times = array("q", (t if isinstance(t, int) else unix_ms(t) for t, _ in pairs))
                values = array("d", (v for _, v in pairs))
            else:
                times, values = array("q", s["times"]), array("d", s["values"])
            batch.append({"labels": dict(s["labels"]), "kind": s["kind"], "times": times, "values": values})
        await self._send(batch)

    async def _send(self, batch: list[dict[str, Any]]) -> None:
        async def attempt(connection: Connection) -> None:
            await connection.session.call(METHODS["metrics.ingest"], MetricsBatch.encode(series=batch))

        await self._link.run("write", attempt)

    async def read(
        self,
        match: Mapping[str, str],
        start: datetime | int,
        end: datetime | int | None = None,
        **limits: int,
    ) -> list[Series]:
        """Every sample of the series a range matches, exactly; start and end in unix milliseconds or datetimes."""
        body = _range(match, start, end, **{f"limit_{k}": v for k, v in limits.items()})

        async def attempt(connection: Connection) -> Any:
            return await download(connection, METHODS["metrics.read"], body)

        joined: list[Series] = []
        for item in (await self._link.run("read", attempt)).items:
            s = MetricsSeries.decode(item)
            labels, kind = s.get("labels", {}), s.get("kind", "gauge")
            if joined and joined[-1].labels == labels:
                joined[-1].times.extend(s.get("times", []))
                joined[-1].values.extend(s.get("values", array("d")))
            else:
                joined.append(Series(labels, kind, list(s.get("times", [])), s.get("values", array("d"))))
        return joined

    async def aggregate(
        self,
        match: Mapping[str, str],
        start: datetime | int,
        end: datetime | int | None,
        width: Duration,
        op: Op,
        **limits: int,
    ) -> list[Aggregate]:
        """Buckets of a width from the range's start, each computed exactly: an increase counts resets."""
        extra = {f"limit_{k}": v for k, v in limits.items()}
        body = _range(match, start, end, width=ms(width), op=op, **extra)

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
            labels = b.get("labels", {})
            if joined and joined[-1].labels == labels:
                joined[-1].buckets.extend(buckets)
            else:
                joined.append(Aggregate(labels, b.get("kind", "gauge"), buckets))
        return joined

    async def drop(self, labels: Mapping[str, str]) -> tuple[bool, int]:
        """Removes one series and everything it holds: whether it was there, and the unreadable groups removed."""

        async def attempt(connection: Connection) -> bytes:
            return await connection.session.call(METHODS["metrics.drop"], MetricsLabels.encode(labels=dict(labels)))

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
                        "labels": {**labels, "__name__": instrument.name},
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
