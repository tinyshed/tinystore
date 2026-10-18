"""A program's instruments, as plan/api/metrics.md has them.

Counters, gauges, a gauge read from a function, timers and histograms, each
set of labels a series of its own. A call changes a number here and never
waits. What changed goes to the store's totals on a stream of its own, a
batch a second, each numbered in this run's session, so that the store
counts it once: a batch whose answer a lost connection took goes again on the
next, and a store that started again is sent nothing it may have counted.
"""

from __future__ import annotations

import asyncio
import contextlib
import inspect
import json
import math
import secrets
import struct
import time
from dataclasses import dataclass, field
from datetime import timedelta
from typing import TYPE_CHECKING, Any, Literal, Self

from ._background import FailureLog, message_of, say_own
from ._session import ConnectionLostError, Stream
from ._time import ms
from ._wire.protocol import MetricsAck, MetricsBatch, MetricsSession, methods
from .errors import InvalidError, OutcomeUnknownError, TinystoreError, UnavailableError

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Coroutine, Mapping, Sequence
    from types import TracebackType

    from ._connection import Link
    from ._time import Duration

LabelValue = str | int | float | bool
"""A label's value as an instrument takes it: a number or a bool is kept as its text."""

Form = Literal["counter", "gauge", "timer", "histogram"]

_MOST_SETS = 2000
"""The sets of labels an instrument holds; past them it counts into the series of __overflow."""
_SEND_EVERY = 1.0
_READ_EVERY = 15.0
"""How often a gauge's function is read, in seconds."""
_MOST_UNANSWERED = 16
"""Batches sent and not yet answered, past which changes gather for the next."""
_MOST_CHANGES = 1000
_IDLE = 3600.0
"""How long a counter's or a timer's set of labels stays without a call, in seconds."""
_MOST_BOUNDS = 128
_LAST_ANSWER = 5.0
"""How long the store's close waits for the answer that ends the instruments' stream, in seconds."""


@dataclass(eq=False)
class _Declared:
    """An instrument as the stream declares it, and its sets of labels."""

    id: int
    name: str
    form: Form
    unit: str | None
    help: str | None
    keep: int | None
    bounds: list[float] | None
    exponential: bool
    sets: dict[str, _Set] = field(default_factory=dict[str, "_Set"])
    overflow: _Set | None = None
    refused: bool = False
    """the store refused it: its calls change nothing, said once"""
    reads: dict[str, _GaugeRead] | None = None
    """a gauge functions read, one a set of labels, by the key of its labels"""


@dataclass(eq=False)
class _GaugeRead:
    labels: dict[str, str]
    read: Callable[[], float | Awaitable[float]]
    reading: bool = False
    failed: str | None = None


@dataclass(eq=False)
class _Set:
    """A set of labels of an instrument: its id on the stream, and a gauge's value."""

    id: int
    instrument: _Declared
    labels: dict[str, str]
    overflow: bool
    touched: float
    value: float | None = None


@dataclass
class _Change:
    """What a set of labels changed since the batch before."""

    add: float | None = None
    value: float | None = None
    count: int = 0
    sum: float = 0.0
    longest: float = 0.0
    buckets: list[int] | None = None
    bins: dict[str, int] | None = None


@dataclass(eq=False)
class _Batch:
    number: int
    changes: dict[_Set, _Change]
    flush: bool
    waiters: list[asyncio.Future[None]] = field(default_factory=list[asyncio.Future[None]])
    sent_on: _Opened | None = None


@dataclass(eq=False)
class _Opened:
    """A stream of the instruments, and what it declared."""

    stream: Stream
    instruments: set[int] = field(default_factory=set[int])
    sets: set[int] = field(default_factory=set[int])
    listening: asyncio.Task[None] | None = None
    """what reads the store's answers, which ends with the stream"""


def _key_of(labels: Mapping[str, str]) -> str:
    return json.dumps(sorted(labels.items()))


def labels_of(given: Mapping[str, LabelValue], timed: bool) -> dict[str, str]:
    """Labels as an instrument keeps them: text, a number or a bool as its text, none of the store's own."""
    labels: dict[str, str] = {}
    for label, value in given.items():
        if not label or len(label.encode()) > 256:
            raise InvalidError(f"label {label!r}: a name of 1 to 256 bytes")
        if label.startswith("__"):
            raise InvalidError(f"label {label}: a name beginning with __ is the store's own")
        if timed and label == "le":
            raise InvalidError("label le: a timer's _bucket series give it")
        text = label_text(label, value)
        if len(text.encode()) > 4096:
            raise InvalidError(f"label {label}: a value of 4096 bytes at most")
        labels[label] = text
    return labels


def label_text(label: str, value: LabelValue) -> str:
    """A label's value as the store keeps it: a number or a bool as its text, 200 as "200", True as "true"."""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, float):
        return _number_text(value)
    if isinstance(value, int | str):  # pyright: ignore[reportUnnecessaryIsInstance]
        return str(value)
    raise InvalidError(f"label {label}: a value of text, a number or a bool, where {value!r} is")


def _number_text(value: float) -> str:
    """A float as JavaScript's String writes it, so that every language writes one label: 5, 0.25, 1e+21."""
    if value.is_integer() and abs(value) < 1e21:
        return str(int(value))
    text = repr(value)
    if "e" in text:
        mantissa, exponent = text.split("e")
        sign = "-" if exponent.startswith("-") else "+"
        return f"{mantissa}e{sign}{exponent.lstrip('+-').lstrip('0') or '0'}"
    return text


def _finite(name: str, what: str, value: float) -> float:
    if isinstance(value, bool) or not isinstance(value, int | float) or not math.isfinite(value):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"metrics {name}: {what} {value!r}, where a finite number is")
    return float(value)


def _sums(declared: _Declared, total: float) -> None:
    """Refuses at the call what would take a send past the largest number, which the store would refuse unsaid."""
    if not math.isfinite(total):
        raise InvalidError(f"metrics {declared.name}: a sum past the largest number a send carries")


def _bounds_of(name: str, bounds: Sequence[float]) -> list[float]:
    if not 1 <= len(bounds) <= _MOST_BOUNDS:
        raise InvalidError(f"metrics {name}: 1 to {_MOST_BOUNDS} bounds")
    for i, bound in enumerate(bounds):
        if not (math.isfinite(bound) and bound >= 0) or (i > 0 and not bound > bounds[i - 1]):
            raise InvalidError(f"metrics {name}: bounds of 0 or more, each above the one before")
    return [float(bound) for bound in bounds]


@dataclass(frozen=True, slots=True)
class _Told:
    unit: str | None
    help: str | None
    keep: int | None
    bounds: list[float] | None
    exponential: bool


def _told_of(
    name: str,
    form: Form,
    unit: str | None,
    help_: str | None,
    keep: Duration | None,
    buckets: Sequence[float] | Literal["exponential"] | None,
) -> _Told:
    if not name or len(name.encode()) > 4096:
        raise InvalidError(f"metrics name {name!r}: 1 to 4096 bytes of UTF-8")
    if unit is not None and len(unit.encode()) > 32:
        raise InvalidError(f"metrics {name}: a unit of 32 bytes at most")
    if help_ is not None and len(help_.encode()) > 1024:
        raise InvalidError(f"metrics {name}: a help of 1024 bytes at most")
    kept = None if keep is None else ms(keep, f"metrics {name}: keep")
    if kept == 0:
        raise InvalidError(f"metrics {name}: keep 0, where a name keeps its samples 1ms at least")
    if buckets is not None and form not in ("timer", "histogram"):
        raise InvalidError(f"metrics {name}: buckets are a timer's or a histogram's")
    if buckets == "exponential":
        return _Told(unit, help_, kept, None, True)
    return _Told(unit, help_, kept, None if buckets is None else _bounds_of(name, buckets), False)


def exponential_buckets(start: float, factor: float, count: int) -> list[float]:
    """Upper bounds from start, each factor times the one before: exponential_buckets(1, 2, 14) is 1, 2, 4 … 8192."""
    if not (start > 0 and factor > 1 and isinstance(count, int) and 1 <= count <= _MOST_BOUNDS):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(
            f"exponential_buckets({start}, {factor}, {count}): a start above 0, a factor above 1, 1 to {_MOST_BOUNDS}"
        )
    bounds = [float(start)]
    while len(bounds) < count:
        bounds.append(bounds[-1] * factor)
    return bounds


# The fractions of Prometheus' native grid for schema 3, 2^(j/8 − 1), as its
# table writes them: three a unit in the last place below the nearest float,
# so that a value on a bound falls where Prometheus and the store put it.
_FRACTIONS = (
    0.5,
    0.5452538663326288,
    0.5946035575013605,
    0.6484197773255048,
    0.7071067811865475,
    0.7711054127039704,
    0.8408964152537144,
    0.9170040432046711,
)


def bin_of(value: float) -> str:
    """The interval of the store's grid a value of 0 or more falls in, by its label: '3:13', or 'zero'."""
    if value == 0:
        return "zero"
    # a subnormal falls in the lowest interval, whose bound is the least normal float
    bits = struct.unpack(">Q", struct.pack(">d", max(value, 2.2250738585072014e-308)))[0]
    exponent = ((bits >> 52) & 0x7FF) - 1022
    fraction = struct.unpack(">d", struct.pack(">Q", (bits & ~(0x7FF << 52)) | (1022 << 52)))[0]
    within = next((i for i, bound in enumerate(_FRACTIONS) if bound >= fraction), 8)
    return f"3:{within + (exponent - 1) * 8}"


class Instruments:
    """The instruments of a store's handle, and what sends their changes; ``store.metrics`` makes them."""

    def __init__(self, link: Link) -> None:
        self._link = link
        self._session = secrets.token_bytes(16)
        self._declared: dict[str, _Declared] = {}
        self._by_id: dict[int, _Declared] = {}
        self._pending: dict[_Set, _Change] = {}
        self._unanswered: list[_Batch] = []
        self._answered = 0
        self._next = 1
        self._ids = 0
        self._opened: _Opened | None = None
        self._opening: asyncio.Future[_Opened] | None = None
        self._pumping: asyncio.Future[None] | None = None
        self._ticking: asyncio.Task[None] | None = None
        self._stopped = False
        self._unreported: BaseException | None = None
        self._failures = FailureLog("metrics instruments")
        self._tasks: set[asyncio.Task[None]] = set()

    def declare(
        self,
        name: str,
        form: Form,
        *,
        unit: str | None = None,
        help_: str | None = None,
        keep: Duration | None = None,
        buckets: Sequence[float] | Literal["exponential"] | None = None,
        read: bool = False,
    ) -> _Declared:
        """An instrument of name, made the first time; another form or other options than it has are refused."""
        told = _told_of(name, form, unit, help_, keep, buckets)
        held = self._declared.get(name)
        if held is not None:
            same = (held.unit, held.help, held.keep, held.bounds, held.exponential) == (
                told.unit,
                told.help,
                told.keep,
                told.bounds,
                told.exponential,
            )
            if held.form != form or not same or read != (held.reads is not None):
                raise InvalidError(
                    f"metrics {name}: another instrument or other options than it has while the store is open"
                )
            return held
        self._ids += 1
        reads: dict[str, _GaugeRead] | None = {} if read else None
        declared = _Declared(
            self._ids, name, form, told.unit, told.help, told.keep, told.bounds, told.exponential, reads=reads
        )
        self._declared[name] = declared
        self._by_id[declared.id] = declared
        self._start()
        return declared

    def reader(
        self, name: str, labels: dict[str, str], read: Callable[[], float | Awaitable[float]], **told: Any
    ) -> None:
        """A gauge of name whose set of labels read gives every 15 seconds; each set has a function of its own."""
        declared = self.declare(name, "gauge", read=True, **told)
        key = _key_of(labels)
        assert declared.reads is not None
        if key in declared.reads:
            raise InvalidError(
                f"metrics {name}: a function reads its gauge of {labels} already while the store is open"
            )
        declared.reads[key] = _GaugeRead(labels, read)

    def set_of(self, instrument: _Declared, labels: dict[str, str]) -> _Set:
        """The set of labels of an instrument, made the first time; past the sets it holds, its __overflow."""
        key = _key_of(labels)
        held = instrument.sets.get(key)
        if held is not None:
            return held
        if len(instrument.sets) >= _MOST_SETS:
            if instrument.overflow is None:
                instrument.overflow = self._labelled(instrument, {}, True)
            return instrument.overflow
        made = self._labelled(instrument, labels, False)
        instrument.sets[key] = made
        return made

    def _labelled(self, instrument: _Declared, labels: dict[str, str], overflow: bool) -> _Set:
        self._ids += 1
        return _Set(self._ids, instrument, labels, overflow, time.monotonic())

    def pending(self, set_: _Set) -> _Change | None:
        return self._pending.get(set_)

    def change(self, set_: _Set) -> _Change:
        """What a call changes of a set, kept for the next batch."""
        set_.touched = time.monotonic()
        self._start()
        change = self._pending.get(set_)
        if change is None:
            instrument = set_.instrument
            change = _Change(
                buckets=None if instrument.bounds is None else [0] * (len(instrument.bounds) + 1),
                bins={} if instrument.exponential else None,
            )
            self._pending[set_] = change
        return change

    async def flush(self) -> bool:
        """Sends what changed now and has the store write its totals; says whether there was anything to send.

        One the store cannot be reached for fails as a call does, and what it
        carried goes once the store can be. What the store refused since the
        flush before, and what a store that started again may have lost, fail
        it, each once.
        """
        if not self._declared:
            return False
        await self._read_gauges()
        done: asyncio.Future[None] = asyncio.get_running_loop().create_future()
        self._batches(True)[-1].waiters.append(done)
        self._pump()
        await done
        return True

    async def stop(self) -> None:
        """Stops the timers, sends what is left, and ends the stream; the store's close calls it."""
        if self._stopped:
            return
        self._stopped = True
        if self._ticking is not None:
            self._ticking.cancel()
        if not self._declared:
            return
        try:
            await self.flush()
        except Exception as err:
            self._failures.failed(err)
        opened = self._opened
        if opened is not None:
            with contextlib.suppress(Exception):
                await opened.stream.send(b"", True)
            # the store ends the stream once it answered the last DATA: a
            # connection closed before then would leave a server draining it
            if opened.listening is not None:
                with contextlib.suppress(Exception):
                    await asyncio.wait_for(asyncio.shield(opened.listening), _LAST_ANSWER)

    def _start(self) -> None:
        """Starts the sends on the running loop; an instrument made before one runs starts them at its first call."""
        if self._ticking is not None or self._stopped:
            return
        try:
            loop = asyncio.get_running_loop()
        except RuntimeError:
            return
        self._ticking = loop.create_task(self._tick_loop())

    def _spawn(self, work: Coroutine[Any, Any, None]) -> asyncio.Task[None]:
        """Runs work beside the calls, held until it ends, since the loop holds a task only weakly."""
        task = asyncio.ensure_future(work)
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)
        return task

    async def _tick_loop(self) -> None:
        last_read = time.monotonic()
        while not self._stopped:
            await asyncio.sleep(_SEND_EVERY)
            if time.monotonic() - last_read >= _READ_EVERY:
                last_read = time.monotonic()
                self._spawn(self._read_gauges())
            self._tick()

    def _tick(self) -> None:
        now = time.monotonic()
        for instrument in self._declared.values():
            if instrument.form == "gauge":
                continue
            for key, set_ in list(instrument.sets.items()):
                if now - set_.touched >= _IDLE and set_ not in self._pending:
                    del instrument.sets[key]
        if self._pending and len(self._unanswered) < _MOST_UNANSWERED:
            self._batches(False)
        self._pump()

    def _batches(self, flush: bool) -> list[_Batch]:
        """What waits, as batches of _MOST_CHANGES changes each; with flush, the last asks the store to write."""
        made: list[_Batch] = []
        pending = list(self._pending.items())
        self._pending = {}
        start = 0
        while start < len(pending) or (flush and not made):
            changes = dict(pending[start : start + _MOST_CHANGES])
            last = start + _MOST_CHANGES >= len(pending)
            made.append(_Batch(self._next, changes, flush and last))
            self._next += 1
            start += _MOST_CHANGES
        self._unanswered.extend(made)
        return made

    def _pump(self) -> None:
        """Sends every unanswered batch not gone on the stream open now, in order, a send at a time."""
        previous = self._pumping

        async def pump() -> None:
            if previous is not None:
                with contextlib.suppress(BaseException):
                    await previous
            await self._send_waiting()

        self._pumping = asyncio.ensure_future(pump())

    async def _send_waiting(self) -> None:
        # a connection lost on the way is dialled again, as a call's is; a store
        # that cannot be dialled is tried again at the next tick, not at once
        tries = 0
        while not self._sent_all():
            try:
                await self._send_on(await self._stream())
                self._failures.succeeded()
                return
            except ConnectionLostError:
                tries += 1
                if tries < 3:
                    continue
                raise
            except Exception as err:
                self._failures.failed(err)
                self._fail(err)
                return

    def _sent_all(self) -> bool:
        return all(batch.sent_on is not None and batch.sent_on is self._opened for batch in self._unanswered)

    async def _send_on(self, opened: _Opened) -> None:
        for batch in list(self._unanswered):
            if batch.sent_on is not opened:
                batch.sent_on = opened
                await opened.stream.send(_encode(batch, opened), False)

    def _fail(self, err: BaseException) -> None:
        """Every flush waiting fails with err; the batches wait for the next send."""
        for batch in self._unanswered:
            for waiter in batch.waiters:
                if not waiter.done():
                    waiter.set_exception(_error_of(err))
            batch.waiters.clear()

    def _drop(self, err: BaseException) -> None:
        """The store refused the batch it was to apply next and would again: it is dropped, said once."""
        if not self._unanswered:
            return
        dropped = self._unanswered.pop(0)
        say_own(
            "warn",
            "metrics instruments dropped a batch the store refused",
            {"changes": len(dropped.changes), "error": message_of(err)},
        )
        self._fail_with(dropped.waiters, _error_of(err))
        self._renumber()

    def _fail_with(self, waiters: list[asyncio.Future[None]], err: BaseException) -> None:
        """Fails the flushes that wait with err, or the next flush when none does."""
        if not waiters and self._unreported is None:
            self._unreported = err
        for waiter in waiters:
            if not waiter.done():
                waiter.set_exception(err)
        waiters.clear()

    async def _stream(self) -> _Opened:
        """The stream open now, or one opened and resumed: the store says what it applied of the session."""
        if self._opened is not None and not self._opened.stream.finished:
            return self._opened
        opening = self._opening
        if opening is None:
            opening = self._opening = asyncio.ensure_future(self._open())
        try:
            return await asyncio.shield(opening)
        finally:
            if self._opening is opening and opening.done():
                self._opening = None

    async def _open(self) -> _Opened:
        connection = await self._link.connection()
        body = MetricsSession.encode(id=self._session, after=self._answered)
        stream = await connection.session.open(methods["metrics.instruments"], body, False)
        head = await stream.next()
        opened = _Opened(stream)
        self._resume(MetricsAck.decode(head.body))
        self._opened = opened
        opened.listening = self._spawn(self._listen(opened))
        return opened

    def _resume(self, ack: dict[str, Any]) -> None:
        """Takes up where the store is.

        A batch it applied is done; one sent that it may have applied, when it
        knows no session, is dropped rather than counted twice, and those never
        sent are numbered on. Every gauge's value goes again, since a gauge
        left with the last connection.
        """
        applied = ack.get("applied", 0)
        if ack.get("known"):
            self._answer(ack)
        else:
            lost = [batch for batch in self._unanswered if batch.sent_on is not None]
            self._unanswered = [batch for batch in self._unanswered if batch.sent_on is None]
            if lost:
                changes = sum(len(batch.changes) for batch in lost)
                say_own(
                    "warn",
                    "metrics instruments dropped what a store that started again may have counted",
                    {"batches": len(lost), "changes": changes},
                )
                unknown = OutcomeUnknownError(
                    f"the store started again: {len(lost)} batches of instruments it may have counted before it "
                    "stopped are dropped rather than counted twice"
                )
                self._fail_with([waiter for batch in lost for waiter in batch.waiters], unknown)
            self._answered = applied
            self._renumber()
        for instrument in self._declared.values():
            if instrument.form != "gauge":
                continue
            for set_ in instrument.sets.values():
                if set_.value is not None:
                    self.change(set_).value = set_.value

    def _renumber(self) -> None:
        """Numbers what waits after the last batch the store answered."""
        self._next = self._answered + 1
        for batch in self._unanswered:
            batch.number = self._next
            self._next += 1

    async def _listen(self, opened: _Opened) -> None:
        """Reads the store's answers on a stream until it ends; what it had not answered goes again on another."""
        try:
            while True:
                event = await opened.stream.next()
                opened.stream.consumed(len(event.body))
                self._answer(MetricsAck.decode(event.body))
                if event.end:
                    return
        except Exception as err:
            if not self._stopped:
                self._failures.failed(err)
            if isinstance(err, InvalidError) and not self._stopped:
                self._drop(err)
        finally:
            if self._opened is opened:
                self._opened = None
            if not self._stopped and self._unanswered:
                self._pump()

    def _answer(self, ack: dict[str, Any]) -> None:
        """An answer: the instruments the store refused, then the batches it applied."""
        for refused in ack.get("refused", []):
            instrument = self._by_id.get(refused.get("instrument", 0))
            if instrument is not None and not instrument.refused:
                instrument.refused = True
                error = refused.get("error", "")
                say_own("warn", "metrics instrument refused", {"instrument": instrument.name, "error": error})
                if self._unreported is None:
                    self._unreported = InvalidError(f"metrics {instrument.name}: the store refused it: {error}")
        applied = ack.get("applied", 0)
        self._answered = max(self._answered, applied)
        while self._unanswered and self._unanswered[0].number <= applied:
            done = self._unanswered.pop(0)
            for waiter in done.waiters:
                failed, self._unreported = self._unreported, None
                if waiter.done():
                    continue
                if failed is None:
                    waiter.set_result(None)
                else:
                    waiter.set_exception(failed)

    async def _read_gauges(self) -> None:
        """Calls each gauge's function whose last call ended; a value it gives goes with the next batch."""
        reads = [
            self._read_gauge(instrument, gauge)
            for instrument in self._declared.values()
            if not instrument.refused
            for gauge in (instrument.reads or {}).values()
            if not gauge.reading
        ]
        await asyncio.gather(*reads)

    async def _read_gauge(self, instrument: _Declared, gauge: _GaugeRead) -> None:
        gauge.reading = True
        try:
            value = gauge.read()
            if inspect.isawaitable(value):
                value = await value
            if isinstance(value, bool) or not isinstance(value, int | float) or not math.isfinite(value):  # pyright: ignore[reportUnnecessaryIsInstance]
                raise InvalidError(f"a gauge's function gave {value!r}, where a finite number is")
            set_ = self.set_of(instrument, gauge.labels)
            set_.value = float(value)
            self.change(set_).value = float(value)
            gauge.failed = None
        except Exception as err:
            error = message_of(err)
            if error != gauge.failed:
                say_own(
                    "warn",
                    "metrics gauge read failed",
                    {"instrument": instrument.name, "labels": json.dumps(gauge.labels), "error": error},
                )
                gauge.failed = error
        finally:
            gauge.reading = False


def _error_of(err: BaseException) -> BaseException:
    return err if isinstance(err, TinystoreError) else UnavailableError(str(err))


def _encode(batch: _Batch, opened: _Opened) -> bytes:
    """A batch's bytes for a stream: the instruments and sets it names that the stream has not declared."""
    instruments: list[dict[str, Any]] = []
    sets: list[dict[str, Any]] = []
    changes: list[dict[str, Any]] = []
    for set_, change in batch.changes.items():
        instrument = set_.instrument
        if instrument.refused:
            continue
        if instrument.id not in opened.instruments:
            opened.instruments.add(instrument.id)
            instruments.append(
                {
                    "number": instrument.id,
                    "name": instrument.name,
                    "form": instrument.form,
                    "unit": instrument.unit,
                    "help": instrument.help,
                    "keep": instrument.keep,
                    "bounds": instrument.bounds,
                    "exponential": instrument.exponential or None,
                }
            )
        if set_.id not in opened.sets:
            opened.sets.add(set_.id)
            sets.append(
                {
                    "number": set_.id,
                    "instrument": instrument.id,
                    "labels": set_.labels,
                    "overflow": set_.overflow or None,
                }
            )
        changes.append(
            {
                "set": set_.id,
                "add": change.add,
                "value": change.value,
                "count": change.count,
                "sum": change.sum,
                "longest": change.longest,
                "buckets": change.buckets,
                "bins": change.bins,
            }
        )
    return MetricsBatch.encode(
        number=batch.number, instruments=instruments, sets=sets, changes=changes, flush=batch.flush or None
    )


class Counter:
    """A total that only goes up, which the store keeps: processes counting into one series count into one total."""

    def __init__(self, instruments: Instruments, declared: _Declared, labels: dict[str, str]) -> None:
        self._instruments = instruments
        self._declared = declared
        self._labels = labels

    def labels(self, **labels: LabelValue) -> Counter:
        """The same counter with more labels, a series of its own; the same labels in any order are one."""
        return Counter(self._instruments, self._declared, {**self._labels, **labels_of(labels, False)})

    def inc(self) -> None:
        self.add(1)

    def add(self, n: float) -> None:
        """Adds n, a finite number of 0 or more; add(0) makes the set of labels known with a total of zero."""
        if _finite(self._declared.name, "a counter adds", n) < 0:
            raise InvalidError(f"metrics {self._declared.name}: a counter adds {n}; it only grows")
        if self._declared.refused:
            return
        set_ = self._instruments.set_of(self._declared, self._labels)
        pending = self._instruments.pending(set_)
        added = (0.0 if pending is None or pending.add is None else pending.add) + n
        _sums(self._declared, added)
        self._instruments.change(set_).add = added


class Gauge:
    """A value that goes up and down, written while this program is connected."""

    def __init__(self, instruments: Instruments, declared: _Declared, labels: dict[str, str]) -> None:
        self._instruments = instruments
        self._declared = declared
        self._labels = labels

    def labels(self, **labels: LabelValue) -> Gauge:
        return Gauge(self._instruments, self._declared, {**self._labels, **labels_of(labels, False)})

    def set(self, value: float) -> None:
        self._to(_finite(self._declared.name, "a gauge set to", value))

    def add(self, n: float) -> None:
        """Moves it by n, from zero when it was never set."""
        _finite(self._declared.name, "a gauge moved by", n)
        set_ = self._instruments.set_of(self._declared, self._labels)
        self._to(_finite(self._declared.name, "a gauge moved to", (set_.value or 0.0) + n))

    def inc(self) -> None:
        self.add(1)

    def dec(self) -> None:
        self.add(-1)

    def _to(self, value: float) -> None:
        if self._declared.refused:
            return
        set_ = self._instruments.set_of(self._declared, self._labels)
        set_.value = value
        self._instruments.change(set_).value = value


def _measured(instruments: Instruments, declared: _Declared, labels: dict[str, str], value: float) -> None:
    """Counts a duration or a value into a timer's or a histogram's change: its count, sum, longest and bucket."""
    if declared.refused:
        return
    set_ = instruments.set_of(declared, labels)
    pending = instruments.pending(set_)
    _sums(declared, (0.0 if pending is None else pending.sum) + value)
    change = instruments.change(set_)
    change.count += 1
    change.sum += value
    change.longest = value if change.count == 1 else max(change.longest, value)
    if change.buckets is not None and declared.bounds is not None:
        bucket = next((i for i, bound in enumerate(declared.bounds) if value <= bound), len(declared.bounds))
        change.buckets[bucket] += 1
    if change.bins is not None:
        found = bin_of(value)
        change.bins[found] = change.bins.get(found, 0) + 1


class Timer:
    """How many things took how long: their count, sum and longest in milliseconds, and their spread."""

    def __init__(self, instruments: Instruments, declared: _Declared, labels: dict[str, str]) -> None:
        self._instruments = instruments
        self._declared = declared
        self._labels = labels

    def labels(self, **labels: LabelValue) -> Timer:
        return Timer(self._instruments, self._declared, {**self._labels, **labels_of(labels, True)})

    def record(self, duration: timedelta | Duration) -> None:
        """Records a duration measured elsewhere: a timedelta, or text such as "250ms"."""
        millis = ms(duration, f"metrics {self._declared.name}: a duration") if isinstance(duration, str) else None
        if millis is None:
            if not isinstance(duration, timedelta):  # pyright: ignore[reportUnnecessaryIsInstance]
                raise InvalidError(f"metrics {self._declared.name}: a duration is a timedelta, not {duration!r}")
            if duration < timedelta(0):
                raise InvalidError(f"metrics {self._declared.name}: a duration of {duration}; it is never negative")
            _measured(self._instruments, self._declared, self._labels, duration / timedelta(milliseconds=1))
            return
        _measured(self._instruments, self._declared, self._labels, float(millis))

    def measure(self) -> Timing:
        """Measures the block it runs: ``async with latency.measure():``, recorded whether it returns or raises."""
        return Timing(self)

    def start(self) -> Timing:
        """A timing that records when it stops, for a span that is no one block."""
        return Timing(self)


class Timing:
    """A span a timer measures from its start; stop records it, once."""

    def __init__(self, timer: Timer) -> None:
        self._timer = timer
        self._started = time.perf_counter()
        self._stopped = False

    def stop(self) -> timedelta:
        """Records how long since the timing started, the first time it is called, and gives it."""
        took = timedelta(seconds=time.perf_counter() - self._started)
        if not self._stopped:
            self._stopped = True
            self._timer.record(took)
        return took

    async def __aenter__(self) -> Self:
        self._started = time.perf_counter()
        return self

    async def __aexit__(
        self, kind: type[BaseException] | None, error: BaseException | None, trace: TracebackType | None
    ) -> None:
        self.stop()

    def __enter__(self) -> Self:
        self._started = time.perf_counter()
        return self

    def __exit__(
        self, kind: type[BaseException] | None, error: BaseException | None, trace: TracebackType | None
    ) -> None:
        self.stop()


class Histogram:
    """A timer of numbers that are not durations: a response's size in bytes."""

    def __init__(self, instruments: Instruments, declared: _Declared, labels: dict[str, str]) -> None:
        self._instruments = instruments
        self._declared = declared
        self._labels = labels

    def labels(self, **labels: LabelValue) -> Histogram:
        return Histogram(self._instruments, self._declared, {**self._labels, **labels_of(labels, True)})

    def record(self, value: float) -> None:
        """Records a value of 0 or more."""
        if _finite(self._declared.name, "a value of", value) < 0:
            raise InvalidError(f"metrics {self._declared.name}: a value of {value}; the lowest bucket starts at 0")
        _measured(self._instruments, self._declared, self._labels, float(value))
