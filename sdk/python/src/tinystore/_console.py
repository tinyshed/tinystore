"""A logger's console line, as Go's records.Handler and the Bun logger write it.

Byte for byte the lines of records/testdata/console.json: pretty for a person
at a terminal, one JSON object a line for a collector.

    11:02:11.123 WARN  api  slow request  requestId=7f3a ms=1200
    {"time":"2026-10-02T11:02:11.123Z","level":"WARN","stream":"api","msg":"slow request","requestId":"7f3a","ms":1200}
"""

from __future__ import annotations

import json
import logging
import os
import re
import sys
from dataclasses import dataclass, field
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any, Literal, TextIO

from ._trace import carried, shared
from ._values import to_json
from .errors import InvalidError

if TYPE_CHECKING:
    from collections.abc import Callable, Iterable

type Console = Literal["pretty", "json", "off"]
"""How a handler writes its lines as they are logged; pretty on a terminal and JSON otherwise when None."""

REDACTED = '"[redacted]"'
"""What a hidden value becomes, in the store and on the console."""


def _no_fields() -> list[tuple[str, str]]:
    return []


@dataclass(slots=True)
class Line:
    """A line as a console writes it and the store keeps it: its fields already JSON."""

    at: int
    """unix nanoseconds"""
    stream: str
    level: int | None = None
    """absent for an event"""
    event: str | None = None
    msg: str | None = None
    context: list[tuple[str, str]] = field(default_factory=_no_fields)
    attrs: list[tuple[str, str]] = field(default_factory=_no_fields)
    trace_id: str | None = None
    """32 hex digits"""
    span_id: str | None = None
    """16 hex digits"""


def _string(text: str) -> str:
    return json.dumps(text, ensure_ascii=False)


def json_line(line: Line) -> str:
    """A line as one JSON object and a newline: time, level, stream, event, msg, context, attrs, trace and span."""
    seconds, nanos = divmod(line.at, 1_000_000_000)
    stamp = datetime.fromtimestamp(seconds, UTC).strftime("%Y-%m-%dT%H:%M:%S")
    out = f'{{"time":"{stamp}.{nanos // 1_000_000:03d}Z"'
    if line.level is not None:
        out += f',"level":{_string(level_label(line.level))}'
    out += f',"stream":{_string(line.stream)}'
    if line.event is not None:
        out += f',"event":{_string(line.event)}'
    if line.msg is not None:
        out += f',"msg":{_string(line.msg)}'
    for key, value in [*line.context, *line.attrs]:
        out += f",{_string(key)}:{value}"
    if line.trace_id is not None:
        out += f',"trace_id":"{line.trace_id}"'
    if line.span_id is not None:
        out += f',"span_id":"{line.span_id}"'
    return out + "}\n"


# the terminal's own sixteen colours, so that its theme chooses the shades
_RESET, _DIM = "\x1b[0m", "\x1b[2m"
_RED, _GREEN, _YELLOW, _BLUE, _MAGENTA, _CYAN = (f"\x1b[{n}m" for n in (31, 32, 33, 34, 35, 36))


def pretty_line(line: Line, color: bool, utc: bool = False) -> str:
    """A line as a person reads it: time of day, level, stream, message, fields, and the trace's first eight digits.

    A value of several lines, a stack or a traceback, follows the line,
    indented. A key or a string shows without its quotes when it is one word
    with no '=', quote or backslash.
    """

    def paint(hue: str, text: str) -> str:
        return hue + text + _RESET if color else text

    seconds, nanos = divmod(line.at, 1_000_000_000)
    moment = datetime.fromtimestamp(seconds, UTC if utc else None)
    label, hue = _pretty_level(line.level)
    out = paint(_DIM, f"{moment:%H:%M:%S}.{nanos // 1_000_000:03d}")
    out += f" {paint(hue, label)}{' ' * max(0, 5 - len(label))} {paint(_CYAN, line.stream)}"
    message = _pretty_message(line)
    if message:
        out += "  " + message
    shown: list[str] = []
    below: list[tuple[str, str]] = []
    for key, value in [*line.context, *line.attrs]:
        text, lines = _pretty_value(value)
        if lines:
            below.append((key, text))
        else:
            shown.append(paint(_DIM, f"{key if _bare(key) else _string(key)}=") + text)
    if line.trace_id is not None:
        shown.append(paint(_DIM, "trace=") + line.trace_id[:8])
    if shown:
        out += "  " + " ".join(shown)
    for key, text in below:
        for i, part in enumerate(text.rstrip("\r\n").split("\n")):
            part = part.removesuffix("\r")
            out += "\n    " + paint(_DIM, f"{key}: {part}" if i == 0 else part)
    return out + "\n"


def level_label(level: int) -> str:
    """A level as slog spells it: INFO, WARN+2, DEBUG-4."""
    for base, start, below in (("DEBUG", -4, 0), ("INFO", 0, 4), ("WARN", 4, 8)):
        if level < below:
            return base if level == start else f"{base}{level - start:+d}"
    return "ERROR" if level == 8 else f"ERROR{level - 8:+d}"


def _pretty_level(level: int | None) -> tuple[str, str]:
    if level is None:
        return "EVENT", _MAGENTA
    hue = _BLUE if level < 0 else _GREEN if level < 4 else _YELLOW if level < 8 else _RED
    return level_label(level), hue


def _pretty_message(line: Line) -> str:
    body = line.msg or ""
    if line.event is None:
        return body
    return f"{line.event}  {body}" if body else line.event


def _pretty_value(value: str) -> tuple[str, bool]:
    if not value.startswith('"'):
        return value, False
    text = json.loads(value)
    if "\n" in text:
        return text, True
    return (text if _bare(text) else value), False


def _bare(text: str) -> bool:
    return text != "" and all(ord(c) > 0x20 and c not in '\x7f"=\\' for c in text)


def redactor(names: Iterable[str]) -> Callable[[str], bool] | None:
    """Whether a key hides its value: the key, or the part of a dotted key after its last dot, is one of names."""
    hidden = {name.lower() for name in names}
    if not hidden:
        return None

    def hides(key: str) -> bool:
        key = key.lower()
        return key in hidden or key.rpartition(".")[2] in hidden

    return hides


def encode(value: Any, hides: Callable[[str], bool] | None) -> str:
    """A value's JSON, an object's keys hidden at any depth; what JSON cannot write is its text, never a lost line."""
    try:
        text = to_json(value)
    except InvalidError:
        return _string(str(value))
    if hides is None or text[:1] not in ("{", "[") or not any(_mentions(text, hides)):
        return text
    return to_json(_hide(json.loads(text), hides))


def _mentions(text: str, hides: Callable[[str], bool]) -> Iterable[bool]:
    # the keys of the text, so that a value needing no change is not decoded
    return (hides(key) for key in re.findall(r'"((?:[^"\\]|\\.)*)"\s*:', text))


def _hide(value: Any, hides: Callable[[str], bool]) -> Any:
    if isinstance(value, dict):
        return {k: "[redacted]" if hides(k) else _hide(v, hides) for k, v in value.items()}  # type: ignore[misc]
    if isinstance(value, list):
        return [_hide(v, hides) for v in value]  # type: ignore[misc]
    return value


def encode_fields(pairs: Iterable[tuple[str, Any]], hides: Callable[[str], bool] | None) -> list[tuple[str, str]]:
    """Fields as key and JSON, a hidden key's value REDACTED."""
    return [(key, REDACTED if hides is not None and hides(key) else encode(value, hides)) for key, value in pairs]


def _forced() -> bool:
    return os.environ.get("FORCE_COLOR", "") not in ("", "0")


def _enable_colours(stream: TextIO) -> bool:
    """Asks a Windows console to read escape sequences, which one from before Windows Terminal prints as text."""
    if sys.platform == "win32":
        import ctypes
        import msvcrt

        try:
            handle = msvcrt.get_osfhandle(stream.fileno())
            mode = ctypes.c_uint32()
            kernel32 = ctypes.windll.kernel32
            if not kernel32.GetConsoleMode(handle, ctypes.byref(mode)):
                return False
            return bool(mode.value & 4 or kernel32.SetConsoleMode(handle, mode.value | 4))
        except (OSError, ValueError, AttributeError):
            return False
    return True


# the attributes every LogRecord has, which are not a line's own fields
_STANDARD = set(logging.LogRecord("", 0, "", 0, "", None, None).__dict__) | {"message", "asctime", "taskName"}


class ConsoleHandler(logging.Handler):
    """A logging.Handler of the console alone: each record is written as it is logged, never kept.

    It writes the lines store.records.handler writes, which keeps them too:
    pretty on a terminal and one JSON object a line otherwise, on stderr
    unless stdout is asked for. A record's logger name is its context, its
    extra fields its attributes, an exception its traceback under error.
    """

    def __init__(
        self,
        stream: str,
        level: int = logging.NOTSET,
        *,
        console: Console | None = None,
        redact: Iterable[str] = (),
        stdout: bool = False,
    ) -> None:
        if not stream:
            raise InvalidError("a handler of no stream")
        if console not in (None, "pretty", "json", "off"):
            raise InvalidError(f"a console of {console!r}, not pretty, json or off")
        super().__init__(level)
        self._stream, self._console, self._stdout = stream, console, stdout
        self._hides = redactor(redact)
        self._colours: bool | None = None

    def emit(self, record: logging.LogRecord) -> None:
        try:
            self._print(self._line(record))
        except Exception:
            self.handleError(record)

    def _line(self, record: logging.LogRecord) -> Line:
        attrs = [(k, v) for k, v in record.__dict__.items() if k not in _STANDARD and not k.startswith("_")]
        if record.exc_info:
            formatter = self.formatter or logging.Formatter()
            attrs.append(("error", formatter.formatException(record.exc_info)))
        if record.stack_info:
            attrs.append(("stack", record.stack_info))
        line = Line(
            at=int(record.created * 1e9),
            stream=self._stream,
            level=(record.levelno - 20) * 4 // 10,
            msg=record.getMessage(),
            context=encode_fields([("logger", record.name), *shared.get()], self._hides),
            attrs=encode_fields(attrs, self._hides),
        )
        trace = carried.get()
        if trace is not None:
            line.trace_id = trace[0].hex()
            line.span_id = None if trace[1] is None else trace[1].hex()
        return line

    def _print(self, line: Line) -> None:
        if self._console == "off":
            return
        out = sys.stdout if self._stdout else sys.stderr
        terminal = out.isatty()
        # FORCE_COLOR other than 0 says a person reads a pipe, as an IDE's run console is one
        forced = _forced()
        if self._console == "json" or (self._console is None and not (terminal or forced)):
            out.write(json_line(line))
        else:
            out.write(pretty_line(line, (terminal or forced) and self._colour(out, forced)))
        out.flush()

    def _colour(self, out: TextIO, forced: bool) -> bool:
        if self._colours is None:
            allowed = not os.environ.get("NO_COLOR") and os.environ.get("TERM") != "dumb"
            self._colours = allowed and (_enable_colours(out) or forced)
        return self._colours


def handler(
    stream: str,
    level: int = logging.NOTSET,
    *,
    console: Console | None = None,
    redact: Iterable[str] = (),
    stdout: bool = False,
) -> ConsoleHandler:
    """A logging.Handler of the console alone, for a program that wants the logger and not the records.

        logging.basicConfig(handlers=[tinystore.handler("app", redact=["password"])], level=logging.INFO)

    store.records.handler(stream) in its place keeps every line too.
    """
    return ConsoleHandler(stream, level, console=console, redact=redact, stdout=stdout)
