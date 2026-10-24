"""The application's own log lines: each printed as it is logged, and kept when the logger is a store's.

A call never waits for the store. Lines wait in a bounded buffer and go
every second, or once half of it waits; what does not fit, or a write that
fails, is dropped and counted, and said in TinyStore's own lines, since no
call is waiting to be told::

    log = store.logger("api")
    log.info("signed in", user=42)
    request = log.bind(request_id=request_id)   # every line of request carries it
    log.event("signup", plan="pro")
"""

from __future__ import annotations

import asyncio
import contextlib
import dataclasses
import inspect
import logging
import os
import sys
import threading
import time
import traceback
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Literal, TextIO, TypedDict

from ._background import FailureLog, say_own
from ._console import SECRETS, SOURCE_KEY, Console, ConsoleTime, Line, Printer, Redactor, encode_fields, redactor
from ._values import to_json
from .config import EnvLayer, env, live
from .errors import InvalidError
from .trace import current_trace

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Iterable, Mapping

secrets = SECRETS
"""The names redact takes to hide the usual secrets: password, token, api key, connection string and the rest."""

Level = Literal["trace", "debug", "info", "warn", "warning", "error"] | int
"""A level: its name, or logging's number for it, logging.INFO among them."""


class Levels(TypedDict, total=False):
    """The least levels a store's logger keeps and prints, each part left out as the option is.

    No store keeps every level, and no console prints what is kept,
    following the config log's changes of it::

        level="info"                                   # keeps info+, prints info+
        level={"store": "info", "console": "warning"}  # keeps info+, prints warning+
        level={"console": "warning"}                   # keeps every level, prints warning+
    """

    store: Level
    """the least level kept; LOG_LEVEL and the config log win over it"""
    console: Level
    """the least level printed; LOG_CONSOLE_LEVEL wins over it"""


_LEVELS = {"trace": -8, "debug": -4, "info": 0, "warn": 4, "warning": 4, "error": 8}
"""slog's numbers, which the store keeps; trace below debug, so that a logger at trace takes every line."""

_FLUSH_EVERY = 1.0
_QUIET = 600.0
"""Lines dropped within this many seconds of the last said are counted rather than said again."""


def level_number(level: Level, what: str = "a logger's level") -> int:
    """A level as slog numbers it: a name, or logging's number, logging.WARNING being 4."""
    if isinstance(level, str):
        found = _LEVELS.get(level.lower())
        if found is None:
            raise InvalidError(f"{what} {level!r}: trace, debug, info, warning or error")
        return found
    if isinstance(level, bool) or not isinstance(level, int):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"{what} {level!r}: a name, or logging's number for it")
    return (level - logging.INFO) * 4 // 10


def _read_env(prefix: str | None) -> dict[str, tuple[str, str]]:
    """LOG_LEVEL, LOG_CONSOLE_LEVEL, LOG_FORMAT and LOG_TIME, each its name after prefix and what it says."""
    read: dict[str, tuple[str, str]] = {}
    for variable in ("LOG_LEVEL", "LOG_CONSOLE_LEVEL", "LOG_FORMAT", "LOG_TIME"):
        name = f"{prefix}_{variable}" if prefix else variable
        text = os.environ.get(name, "").strip()
        if text:
            read[variable] = (name, text)
    return read


_said_ignored: set[str] = set()
"""The values said to be ignored, so that a program making many loggers says each once."""


@dataclass
class _Shared:
    """What a logger and the loggers its bind makes share: one buffer, one timer, one count, one console."""

    append: Callable[[list[Line]], Awaitable[None]] | None
    """None for a logger of the console alone"""
    stream: str
    most: int
    kept: int
    """the least level kept, which the config log changes while the logger runs"""
    printed: int | None
    """the least level printed, LOG_CONSOLE_LEVEL's or level's console part; None prints what is kept"""
    redact: Redactor | None
    replace: Callable[[str, Any], Any] | None
    source: bool
    printer: Printer
    loop: asyncio.AbstractEventLoop | None
    lock: threading.Lock = field(default_factory=threading.Lock)
    waiting: list[Line] = field(default_factory=list[Line])
    timer: asyncio.TimerHandle | None = None
    writing: asyncio.Future[None] | None = None
    dropped: int = 0
    unsaid: int = 0
    said_at: float | None = None
    failures: FailureLog | None = None


@dataclass(frozen=True, slots=True)
class LoggerOptions:
    """What a logger is told; each None takes its default, and each of LOG_LEVEL and the rest wins over its own."""

    level: Level | Levels | None = None
    """one level kept and printed, or a store's logger's two apart"""
    console: Console | None = None
    time: ConsoleTime | None = None
    hide_stream: bool = False
    to: TextIO | None = None
    redact: Iterable[str] = ()
    keep_url_passwords: bool = False
    replace: Callable[[str, Any], Any] | None = None
    source: bool = False
    env: str | bool = True
    """a prefix, "APP" reading APP_LOG_LEVEL and the rest; True the bare names, False none"""
    buffer: int = 1024


@dataclass
class _Settings:
    """A logger's levels, console and time, as its options and then LOG_LEVEL and the rest give them."""

    kept: int
    printed: int | None
    console: Console | None
    shown: ConsoleTime | None
    ignored: list[tuple[str, str, str]] = field(default_factory=list[tuple[str, str, str]])
    """each variable that said what none can mean, with what it said and what it may say"""
    spelled: dict[str, str] = field(default_factory=dict[str, str])
    """each variable read, as the config log spells it"""


def _parts_of(level: Level | Levels | None) -> tuple[int | None, int | None]:
    """A level's two parts as slog numbers them, the least kept and the least printed.

    One level is the kept alone, which the console then follows.
    """
    if not isinstance(level, dict):
        return None if level is None else level_number(level), None
    unknown = sorted(set(level) - {"store", "console"})
    if unknown:
        raise InvalidError(f"a logger's level of {unknown[0]!r}: its parts are store and console")
    store, shown = level.get("store"), level.get("console")
    kept = None if store is None else level_number(store)
    return kept, None if shown is None else level_number(shown, "a logger's console level")


def _settings_of(options: LoggerOptions) -> _Settings:
    """The options with each of LOG_LEVEL and the rest over its own; a console the code turned off stays off."""
    kept, printed = _parts_of(options.level)
    settings = _Settings(
        kept=-(1 << 31) if kept is None else kept,
        printed=printed,
        console=options.console,
        shown=options.time,
    )
    if options.env is False:
        return settings
    for variable, (name, text) in _read_env(None if options.env is True else options.env).items():
        known = text.lower()
        if variable in ("LOG_LEVEL", "LOG_CONSOLE_LEVEL") and known not in _LEVELS:
            settings.ignored.append((name, text, "trace, debug, info, warn or error"))
        elif variable == "LOG_LEVEL":
            settings.kept = _LEVELS[known]
            settings.spelled[name] = "warn" if known == "warning" else known
        elif variable == "LOG_CONSOLE_LEVEL":
            # one process's, which no setting of the config log holds
            settings.printed = _LEVELS[known]
        elif variable == "LOG_FORMAT":
            if known not in ("pretty", "json", "off"):
                settings.ignored.append((name, text, "pretty, json or off"))
            elif settings.console != "off":
                settings.console = known  # type: ignore[assignment]
                settings.spelled[name] = known
        elif known in ("clock", "full", "off"):
            settings.shown = known  # type: ignore[assignment]
            settings.spelled[name] = known
        else:
            settings.ignored.append((name, text, "clock, full or off"))
    return settings


def _shared_of(
    append: Callable[[list[Line]], Awaitable[None]] | None, stream: str, options: LoggerOptions
) -> tuple[_Shared, dict[str, str]]:
    """What a logger shares with the loggers its bind makes, and the variables it read as the config log spells them."""
    if not stream:
        raise InvalidError("a logger of no stream")
    if options.console not in (None, "pretty", "json", "off"):
        raise InvalidError(f"a console of {options.console!r}, not pretty, json or off")
    if options.time not in (None, "clock", "full", "off"):
        raise InvalidError(f"a time of {options.time!r}, not clock, full or off")
    if isinstance(options.buffer, bool) or not isinstance(options.buffer, int) or options.buffer < 1:  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"a logger's buffer of {options.buffer!r} lines")
    settings = _settings_of(options)
    try:
        loop: asyncio.AbstractEventLoop | None = asyncio.get_running_loop()
    except RuntimeError:
        loop = None
    shared = _Shared(
        append=append,
        stream=stream,
        most=options.buffer,
        kept=settings.kept,
        printed=settings.printed,
        redact=redactor(options.redact, options.keep_url_passwords),
        replace=options.replace,
        source=options.source,
        printer=Printer(settings.console, settings.shown, options.hide_stream, options.to),
        loop=loop,
        failures=None if append is None else FailureLog(f"records flush {stream}"),
    )
    for name, value, expected in settings.ignored:
        if shared.printer.off or f"{name}={value}" in _said_ignored:
            continue
        _said_ignored.add(f"{name}={value}")
        attrs = encode_fields([("value", value), ("expected", expected)], None)
        shared.printer.print(
            Line(at=time.time_ns(), stream="tinystore", level=4, msg=f"{name} is ignored", attrs=attrs)
        )
    return shared, settings.spelled


def _shown(value: Any) -> Any:
    """A field as it is kept: an exception as its traceback, which JSON would write as nothing."""
    if isinstance(value, BaseException):
        return "".join(traceback.format_exception(value)).rstrip("\n")
    return value


class Logger:
    """A logger of one stream, as ``store.logger`` and ``tinystore.logger`` make it.

    ``info``, ``warning`` and the rest return at once; a line goes to the
    console as it is logged, and when the logger is a store's becomes an
    event named log, its message the line's text, the call's fields its
    fields and bind's its context.
    """

    def __init__(self, shared: _Shared, context: list[tuple[str, str]]) -> None:
        self._shared = shared
        self._context = context

    @property
    def stream(self) -> str:
        return self._shared.stream

    @property
    def dropped(self) -> int:
        """The lines dropped since the logger began: a full buffer, or a write that failed."""
        return self._shared.dropped

    def debug(self, message: str, /, **fields: Any) -> None:
        self._log(-4, message, fields)

    def info(self, message: str, /, **fields: Any) -> None:
        self._log(0, message, fields)

    def warning(self, message: str, /, **fields: Any) -> None:
        self._log(4, message, fields)

    warn = warning

    def error(self, message: str, /, **fields: Any) -> None:
        self._log(8, message, fields)

    def exception(self, message: str, /, **fields: Any) -> None:
        """An error, the exception being handled kept as its traceback under error."""
        caught = sys.exception()
        self._log(8, message, {"error": caught, **fields} if caught is not None else fields)

    def event(self, name: str, /, **fields: Any) -> None:
        """An event of the stream: named for itself, with no level and no message, written as a line is."""
        if not name:
            raise InvalidError("an event of no name")
        self._emit(None, name, None, fields)

    def bind(self, **context: Any) -> Logger:
        """The logger of the same stream, buffer and console, its context these fields besides its own."""
        shared = self._shared
        encoded = encode_fields(((k, _shown(v)) for k, v in context.items()), shared.redact, shared.replace)
        return Logger(shared, [*self._context, *encoded])

    async def flush(self) -> None:
        """Hands what the buffer holds to the store now, as the timer does every second."""
        await flush(self._shared)

    async def stop(self) -> None:
        """Hands over what the logger holds and stops its timer, as the store's close does."""
        shared = self._shared
        if shared.timer is not None:
            shared.timer.cancel()
            shared.timer = None
        await flush(shared)

    def take_record(self, record: logging.LogRecord, formatter: logging.Formatter) -> None:
        """Prints and keeps a logging record as the logger's levels say.

        Its extra are its fields, and its logger's name is in its context.
        """
        shared = self._shared
        level = (record.levelno - logging.INFO) * 4 // 10
        if not _wanted(shared, level):
            return
        attrs = [(k, v) for k, v in record.__dict__.items() if k not in _STANDARD and not k.startswith("_")]
        if record.exc_info:
            attrs.append(("error", formatter.formatException(record.exc_info)))
        if record.stack_info:
            attrs.append(("stack", record.stack_info))
        fields = encode_fields(attrs, shared.redact, shared.replace)
        if shared.source:
            where = {"function": record.funcName, "file": record.pathname, "line": record.lineno}
            fields.append((SOURCE_KEY, to_json(where)))
        line = Line(
            at=int(record.created * 1e9),
            stream=shared.stream,
            level=level,
            msg=record.getMessage(),
            context=[*self._context, *encode_fields([("logger", record.name)], shared.redact)],
            attrs=fields,
        )
        traced = current_trace()
        if traced is not None:
            line.trace_id, line.span_id = traced
        self.take(line)

    def take(self, line: Line) -> None:
        """Prints a line made elsewhere and keeps it, from any thread, never waiting.

        It is printed from the level printed and kept from the level kept; an
        event, which has no level, is printed and kept.
        """
        shared = self._shared
        if line.level is None or _prints(shared, line.level):
            shared.printer.print(line)
        if line.level is None or _keeps(shared, line.level):
            _queue(shared, line)

    def _log(self, level: int, message: str, fields: Mapping[str, Any]) -> None:
        if not _wanted(self._shared, level):
            return
        self._emit(level, None, message, fields)

    def _emit(self, level: int | None, event: str | None, message: str | None, fields: Mapping[str, Any]) -> None:
        shared = self._shared
        attrs = encode_fields(((k, _shown(v)) for k, v in fields.items()), shared.redact, shared.replace)
        if shared.source:
            attrs.append((SOURCE_KEY, _source_of_caller()))
        line = Line(
            at=time.time_ns(),
            stream=shared.stream,
            level=level,
            event=event,
            msg=message,
            context=self._context,
            attrs=attrs,
        )
        traced = current_trace()
        if traced is not None:
            line.trace_id, line.span_id = traced
        self.take(line)


def _prints(shared: _Shared, level: int) -> bool:
    """Whether a logger prints a line of level: it has a console, and the level is the level printed or above."""
    return not shared.printer.off and level >= (shared.kept if shared.printed is None else shared.printed)


def _keeps(shared: _Shared, level: int) -> bool:
    """Whether a logger keeps a line of level: it is a store's, and the level is its level or above."""
    return shared.append is not None and level >= shared.kept


def _wanted(shared: _Shared, level: int) -> bool:
    """Whether a line of level goes anywhere the logger has, so that one no place takes is never encoded."""
    return _prints(shared, level) or _keeps(shared, level)


def _queue(shared: _Shared, line: Line) -> None:
    """Holds a line for the store, or drops and counts it when the buffer holds no room."""
    if shared.append is None:
        return
    with shared.lock:
        if len(shared.waiting) >= shared.most:
            shared.dropped += 1
            shared.unsaid += 1
            return
        shared.waiting.append(line)
        half = len(shared.waiting) >= shared.most // 2
    loop = shared.loop
    if loop is None or loop.is_closed():
        return
    try:
        running = asyncio.get_running_loop()
    except RuntimeError:
        running = None
    if running is loop:
        _schedule(shared, half)
    else:
        with contextlib.suppress(RuntimeError):
            loop.call_soon_threadsafe(_schedule, shared, half)


def _schedule(shared: _Shared, now: bool) -> None:
    """Starts the second's timer for what waits, or a write at once when half the buffer waits."""
    loop = shared.loop
    assert loop is not None
    if now and shared.writing is None:
        shared.writing = asyncio.ensure_future(flush(shared))
    elif shared.timer is None:
        shared.timer = loop.call_later(_FLUSH_EVERY, _timed, shared)


def _timed(shared: _Shared) -> None:
    shared.timer = None
    if shared.writing is None:
        shared.writing = asyncio.ensure_future(flush(shared))


async def flush(shared: _Shared) -> None:
    """Writes what the buffer holds, and again while half of it filled during the write before.

    A full buffer drops a line before the line could ask for a write, so a
    burst that filled it while a write ran would otherwise wait for the
    timer's next second, every line until then dropped.
    """
    writing = shared.writing
    if writing is not None and writing is not asyncio.current_task():
        with contextlib.suppress(Exception):
            await asyncio.shield(writing)
    _say_dropped(shared)
    append = shared.append
    if append is None:
        return
    try:
        while True:
            with shared.lock:
                batch, shared.waiting = shared.waiting, []
            if not batch:
                return
            for piece in _pieces(batch):
                try:
                    await append(piece)
                    if shared.failures is not None:
                        shared.failures.succeeded()
                except Exception as err:
                    shared.dropped += len(piece)
                    if shared.failures is not None:
                        shared.failures.failed(err)
            with shared.lock:
                if len(shared.waiting) < shared.most // 2:
                    break
        # what came while the write ran waits for the next second
        if shared.waiting and shared.timer is None and shared.loop is not None:
            shared.timer = shared.loop.call_later(_FLUSH_EVERY, _timed, shared)
    finally:
        if shared.writing is asyncio.current_task():
            shared.writing = None


_PIECE = 384 * 1024
"""The text a write carries at most. A request's body holds 2 MiB, and a
buffer of long lines, or one made larger, held more: the whole write was
refused and every line in it dropped. Counted in characters, four bytes each
at worst, so a piece stays within 1.5 MiB however its text is spelt."""


def _pieces(lines: list[Line]) -> list[list[Line]]:
    """The lines in writes of at most _PIECE of text each, in their order."""
    out: list[list[Line]] = []
    piece: list[Line] = []
    size = 0
    for line in lines:
        weight = 64 + len(line.msg or "") + len(line.event or "")
        weight += sum(len(key) + len(value) for key, value in line.attrs)
        weight += sum(len(key) + len(value) for key, value in line.context)
        if piece and size + weight > _PIECE:
            out.append(piece)
            piece, size = [], 0
        piece.append(line)
        size += weight
    if piece:
        out.append(piece)
    return out


def _say_dropped(shared: _Shared) -> None:
    """Says the lines dropped for want of room, at most once in a quiet period: a burst is one line."""
    with shared.lock:
        now = time.monotonic()
        if shared.unsaid == 0 or (shared.said_at is not None and now - shared.said_at < _QUIET):
            return
        dropped, shared.unsaid, shared.said_at = shared.unsaid, 0, now
    say_own("warn", "log lines dropped", {"logger": shared.stream, "dropped": dropped, "buffer": shared.most})


def new_logger(append: Callable[[list[Line]], Awaitable[None]] | None, stream: str, options: LoggerOptions) -> Logger:
    """A logger of one stream, its lines appended through append when it is a store's."""
    return Logger(_shared_of(append, stream, options)[0], [])


@dataclass(frozen=True, slots=True)
class LogConfig:
    """What a store's logger declares its config log with, and how it takes the level each value gives.

    The code's options are the defaults, trace for a logger told no level,
    and the variables it read the layer over them.
    """

    layers: list[Any]
    take: Callable[[str], None]


_LEVEL_NAMES = ("trace", "debug", "info", "warn", "error")
_declarations: dict[tuple[str, str, str], type] = {}
"""The config log's declarations made, one a set of defaults, so that loggers told alike share one config."""


def new_store_logger(
    append: Callable[[list[Line]], Awaitable[None]], stream: str, options: LoggerOptions
) -> tuple[Logger, LogConfig]:
    """A store's logger of one stream, and what it declares its config log with."""
    shared, spelled = _shared_of(append, stream, options)
    least, _ = _parts_of(options.level)
    level = "trace" if least is None else next((n for n in _LEVEL_NAMES if _LEVELS[n] >= least), "error")
    pretty = Printer(None, None, False, options.to).pretty()
    shown = options.console or ("pretty" if pretty else "json")
    declaration = _log_declaration(level, shown, options.time or "clock")

    def take(name: str) -> None:
        # the level kept, and printed too while the level has no console part
        if name in _LEVELS:
            shared.kept = _LEVELS[name]

    made = Logger(shared, [])
    if options.env is False:
        return made, LogConfig([declaration], take)
    prefix = "LOG" if options.env is True else f"{options.env}_LOG"
    layer: EnvLayer = env(prefix, from_=spelled)
    return made, LogConfig([declaration, layer], take)


def _log_declaration(level: str, shown: str, time: str) -> type:
    """The config log as a logger declares it: its level live, each setting one of what it may be, its own first."""
    key = (level, shown, time)
    found = _declarations.get(key)
    if found is None:

        def one_of(first: str, every: tuple[str, ...]) -> Any:
            return Literal[(first, *(name for name in every if name != first))]  # pyright: ignore[reportInvalidTypeArguments]

        found = dataclasses.make_dataclass(
            "Log",
            [
                ("level", one_of(level, _LEVEL_NAMES), live(level)),
                ("format", one_of(shown, ("pretty", "json", "off")), shown),
                ("time", one_of(time, ("clock", "full", "off")), time),
            ],
        )
        _declarations[key] = found
    return found


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
) -> Logger:
    """A logger of the console alone, for a program that wants the logger and no store::

        log = tinystore.logger("app", redact=tinystore.secrets)

    ``store.logger("app")`` in its place keeps every line too.
    """
    options = LoggerOptions(level, console, time, hide_stream, to, redact, keep_url_passwords, replace, source, env)
    return new_logger(None, stream, options)


# the attributes every LogRecord has, which are not a line's own fields
_STANDARD = set(logging.LogRecord("", 0, "", 0, "", None, None).__dict__) | {"message", "asctime", "taskName"}


class LogHandler(logging.Handler):
    """A logging.Handler that logs into a logger: a record's extra its fields, its logger's name its context."""

    def __init__(self, into: Logger, level: int = logging.NOTSET) -> None:
        super().__init__(level)
        self._into = into

    @property
    def logger(self) -> Logger:
        """The logger it logs into, whose flush hands its lines over now."""
        return self._into

    def emit(self, record: logging.LogRecord) -> None:
        try:
            self._into.take_record(record, self.formatter or logging.Formatter())
        except Exception:
            self.handleError(record)


def _source_of_caller() -> str:
    """Where a line was logged, as slog's handlers spell a source: the first frame outside this file."""
    here = os.path.normcase(__file__)
    frame = inspect.currentframe()
    while frame is not None and os.path.normcase(frame.f_code.co_filename) == here:
        frame = frame.f_back  # type: ignore[assignment]
    if frame is None:
        return "{}"
    return to_json({"function": frame.f_code.co_name, "file": frame.f_code.co_filename, "line": frame.f_lineno})
