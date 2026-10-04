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
from datetime import UTC, datetime, timedelta
from time import time_ns
from typing import TYPE_CHECKING, Any, Literal, TextIO, cast

from ._trace import carried, shared
from ._values import to_json
from .config import read_env_files
from .errors import InvalidError

if TYPE_CHECKING:
    from collections.abc import Callable, Iterable

    from .config import FromEnv

type Console = Literal["pretty", "json", "off"]
"""How a handler writes its lines as they are logged; pretty on a terminal and JSON otherwise when None."""

type ConsoleTime = Literal["clock", "full", "off"]
"""How a pretty line shows its time: 11:02:11.123, 2026-10-02 11:02:11.123 +03:00, or none."""

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


def pretty_line(
    line: Line, color: bool, *, utc: bool = False, time: ConsoleTime | None = None, hide_stream: bool = False
) -> str:
    """A line as a person reads it: time of day, level, stream, message, fields, and the trace's first eight digits.

    A value of several lines, a stack or a traceback, follows the line,
    indented. A key or a string shows without its quotes when it is one word
    with no '=', quote or backslash.
    """

    def paint(hue: str, text: str) -> str:
        return hue + text + _RESET if color else text

    label, hue = _pretty_level(line.level)
    out = "" if time == "off" else paint(_DIM, _stamp(line.at, utc, full=time == "full")) + " "
    out += paint(hue, label)
    # the level is padded to five; the columns after it are two spaces apart
    columns = [] if hide_stream else [paint(_CYAN, line.stream)]
    message = _pretty_message(line)
    if message:
        columns.append(message)
    shown: list[str] = []
    below: list[tuple[str, str]] = []
    for key, value in [*line.context, *line.attrs]:
        text, lines = _pretty_value(key, value)
        if lines:
            below.append((key, text))
        else:
            shown.append(paint(_DIM, f"{key if _bare(key) else _string(key)}=") + text)
    if line.trace_id is not None:
        shown.append(paint(_DIM, "trace=") + line.trace_id[:8])
    if shown:
        columns.append(" ".join(shown))
    if columns:
        out += " " * max(0, 5 - len(label)) + " " + "  ".join(columns)
    for key, text in below:
        for i, part in enumerate(text.rstrip("\r\n").split("\n")):
            part = part.removesuffix("\r")
            out += "\n    " + paint(_DIM, f"{key}: {part}" if i == 0 else part)
    return out + "\n"


def _stamp(at: int, utc: bool, *, full: bool) -> str:
    """The time of day, or with the date and zone in full: 2026-10-02 11:02:11.123 +03:00."""
    seconds, nanos = divmod(at, 1_000_000_000)
    moment = datetime.fromtimestamp(seconds, UTC) if utc else datetime.fromtimestamp(seconds).astimezone()
    clock = f"{moment:%H:%M:%S}.{nanos // 1_000_000:03d}"
    if not full:
        return clock
    offset = int((moment.utcoffset() or timedelta()).total_seconds()) // 60
    sign = "-" if offset < 0 else "+"
    return f"{moment.year:04d}-{moment:%m-%d} {clock} {sign}{abs(offset) // 60:02d}:{abs(offset) % 60:02d}"


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


SOURCE_KEY = "source"
"""The field where a line says where it was logged, as slog's handlers spell it."""


def _short_source(value: str) -> str | None:
    """A source as a pretty line shows it, its file's directory and name and its line: server/main.py:42."""
    if not value.startswith("{"):
        return None
    source: dict[str, Any] = json.loads(value)
    file, line = source.get("file"), source.get("line")
    if not isinstance(file, str) or not file:
        return None
    parts = [part for part in re.split(r"[\\/]+", file) if part]
    return "/".join(parts[-2:]) + f":{line if isinstance(line, int) else 0}"


def _pretty_value(key: str, value: str) -> tuple[str, bool]:
    if key == SOURCE_KEY and (place := _short_source(value)) is not None:
        return place, False
    if not value.startswith('"'):
        return value, False
    text = json.loads(value)
    if "\n" in text:
        return text, True
    return (text if _bare(text) else value), False


def _bare(text: str) -> bool:
    return text != "" and all(ord(c) > 0x20 and c not in '\x7f"=\\' for c in text)


SECRETS = (
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "credential",
    "credentials",
    "authorization",
    "cookie",
    "api key",
    "private key",
    "secret key",
    "access key",
    "signing key",
    "encryption key",
    "connection string",
    "dsn",
)
"""The names redact takes to hide the usual secrets, as Go's console.Secrets and Bun's secrets."""


def words(key: str) -> list[str]:
    """A key's words as a person reads them, in lower case: split at anything but a letter or a digit, and where a
    capital begins a word, as in passwordHash and APIKey."""
    out: list[str] = []
    start = -1
    for i, c in enumerate(key):
        if not c.isalnum():
            if start >= 0:
                out.append(key[start:i].lower())
                start = -1
            continue
        before = key[i - 1] if i > 0 else ""
        after = key[i + 1] if i + 1 < len(key) else ""
        begins = c.isupper() and (before.islower() or before.isdigit() or (before.isupper() and after.islower()))
        if start >= 0 and begins:
            out.append(key[start:i].lower())
            start = i
        if start < 0:
            start = i
    if start >= 0:
        out.append(key[start:].lower())
    return out


def _joins_to(key_words: list[str], name: str) -> bool:
    for i in range(len(key_words)):
        rest = name
        for word in key_words[i:]:
            if not rest.startswith(word):
                break
            rest = rest[len(word) :]
            if not rest:
                return True
    return False


@dataclass(frozen=True, slots=True)
class Redactor:
    """What a handler hides: the keys its names name, and a URL's password unless it keeps them."""

    names: tuple[str, ...]
    """each name's words written together, "api key" as apikey"""
    urls: bool

    def hides(self, key: str) -> bool:
        """Whether a key names a secret: some of its words in a row, written together, are a name's."""
        if not self.names:
            return False
        key_words = words(key)
        return any(_joins_to(key_words, name) for name in self.names)


def redactor(names: Iterable[str], keep_url_passwords: bool = False) -> Redactor | None:
    """A handler's Redactor, or None when it hides nothing: "api key" hides api_key and apiKey, "token" bot_token."""
    joined = tuple(j for j in ("".join(words(name)) for name in names) if j)
    if not joined and keep_url_passwords:
        return None
    return Redactor(joined, not keep_url_passwords)


def hide_url_passwords(text: str) -> str:
    """JSON text with the password of each URL inside it hidden, everything else as it was spelled.

    "postgres://ann:hunter2@db/app" is "postgres://ann:[redacted]@db/app".
    """
    out: list[str] = []
    written = 0
    start = 0
    while start < len(text):
        scheme_end = text.find("://", start)
        if scheme_end < 0:
            break
        password, at, end = _find_password(text, scheme_end + 3)
        start = end
        if password < 0 or not _ends_in_scheme(text[:scheme_end]):
            continue
        out.append(text[written:password] + "[redacted]")
        written = at
    if written == 0:
        return text
    return "".join(out) + text[written:]


def _find_password(text: str, start: int) -> tuple[int, int, int]:
    """Where an authority's password begins and the '@' after it, -1 when it has none, and where it ends."""
    at = colon = -1
    i = start
    while i < len(text):
        c = text[i]
        if c == "\\":
            i += 2
            continue
        if c in '/?#"' or c <= " ":
            break
        if c == "@":
            at = i
        elif c == ":" and colon < 0:
            colon = i
        i += 1
    end = min(i, len(text))
    if at < 0 or colon < 0 or colon + 1 >= at:
        return -1, -1, end
    return colon + 1, at, end


def _ends_in_scheme(text: str) -> bool:
    i = len(text)
    while i > 0 and (text[i - 1].isascii() and (text[i - 1].isalnum() or text[i - 1] in "+-.")):
        i -= 1
    return i < len(text) and text[i].isascii() and text[i].isalpha()


def encode(value: Any, redact: Redactor | None) -> str:
    """A value's JSON, an object's secret keys hidden at any depth and a URL's password unless kept; what JSON
    cannot write is its text, never a lost line."""
    try:
        text = to_json(value)
    except InvalidError:
        return _string(str(value))
    if redact is None:
        return text
    if redact.names and text[:1] in ("{", "[") and any(_mentions(text, redact.hides)):
        text = to_json(_hide(json.loads(text), redact.hides))
    return hide_url_passwords(text) if redact.urls else text


def _mentions(text: str, hides: Callable[[str], bool]) -> Iterable[bool]:
    # the keys of the text, so that a value needing no change is not decoded
    return (hides(key) for key in re.findall(r'"((?:[^"\\]|\\.)*)"\s*:', text))


def _hide(value: Any, hides: Callable[[str], bool]) -> Any:
    if isinstance(value, dict):
        return {k: "[redacted]" if hides(k) else _hide(v, hides) for k, v in value.items()}  # type: ignore[misc]
    if isinstance(value, list):
        return [_hide(v, hides) for v in value]  # type: ignore[misc]
    return value


def encode_fields(
    pairs: Iterable[tuple[str, Any]],
    redact: Redactor | None,
    replace: Callable[[str, Any], Any] | None = None,
) -> list[tuple[str, str]]:
    """Fields as key and JSON, each value replaced first when replace is given, a secret key's value REDACTED."""
    fields: list[tuple[str, str]] = []
    for key, given in pairs:
        value = given if replace is None else replace(key, given)
        fields.append((key, REDACTED if redact is not None and redact.hides(key) else encode(value, redact)))
    return fields


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

_LEVELS = {
    "debug": logging.DEBUG,
    "info": logging.INFO,
    "warn": logging.WARNING,
    "warning": logging.WARNING,
    "error": logging.ERROR,
}

type _Unread = tuple[str, str, str]


def _from_environment(
    env: FromEnv | bool, level: int, console: Console | None, time: ConsoleTime | None
) -> tuple[int, Console | None, ConsoleTime | None, list[_Unread]]:
    """LOG_LEVEL, LOG_FORMAT and LOG_TIME, after env's prefix, over the arguments; a console turned off stays off."""
    ignored: list[_Unread] = []
    if env is False:
        return level, console, time, ignored
    prefix, files = ("", ()) if env is True else (env.prefix, env.files)
    variables = read_env_files(files)
    variables.update(os.environ)

    def variable(name: str) -> tuple[str, str | None]:
        full = f"{prefix}_{name}" if prefix else name
        return full, variables.get(full, "").strip() or None

    name, text = variable("LOG_LEVEL")
    if text is not None:
        if text.lower() in _LEVELS:
            level = _LEVELS[text.lower()]
        else:
            ignored.append((name, text, "debug, info, warn or error"))
    name, text = variable("LOG_FORMAT")
    if text is not None:
        if text.lower() not in ("pretty", "json", "off"):
            ignored.append((name, text, "pretty, json or off"))
        elif console != "off":
            console = cast("Console", text.lower())
    name, text = variable("LOG_TIME")
    if text is not None:
        if text.lower() in ("clock", "full", "off"):
            time = cast("ConsoleTime", text.lower())
        else:
            ignored.append((name, text, "clock, full or off"))
    return level, console, time, ignored


_said_ignored: set[str] = set()
"""The values said to be ignored, so that a program making many handlers says each once."""


class ConsoleHandler(logging.Handler):
    """A logging.Handler of the console alone: each record is written as it is logged, never kept.

    It writes the lines store.records.handler writes, which keeps them too:
    pretty on a terminal and one JSON object a line otherwise, on stderr
    unless to says where. LOG_LEVEL, LOG_FORMAT and LOG_TIME win over the
    arguments, or the variables of env's prefix, and env=False reads none. A
    record's logger name is its context, its extra fields its attributes, an
    exception its traceback under error. redact hides the fields whose keys
    name a secret, SECRETS the usual ones, and a URL's password is hidden
    unless keep_url_passwords; replace changes each value first; source adds
    where the line was logged.
    """

    def __init__(
        self,
        stream: str,
        level: int = logging.NOTSET,
        *,
        console: Console | None = None,
        time: ConsoleTime | None = None,
        hide_stream: bool = False,
        to: TextIO | None = None,
        redact: Iterable[str] = (),
        keep_url_passwords: bool = False,
        replace: Callable[[str, Any], Any] | None = None,
        env: FromEnv | bool = True,
        source: bool = False,
    ) -> None:
        if not stream:
            raise InvalidError("a handler of no stream")
        if console not in (None, "pretty", "json", "off"):
            raise InvalidError(f"a console of {console!r}, not pretty, json or off")
        if time not in (None, "clock", "full", "off"):
            raise InvalidError(f"a time of {time!r}, not clock, full or off")
        level, console, time, ignored = _from_environment(env, level, console, time)
        super().__init__(level)
        self._stream, self._console, self._hide_stream, self._to = stream, console, hide_stream, to
        self._time: ConsoleTime | None = time
        self._redact = redactor(redact, keep_url_passwords)
        self._replace, self._source = replace, source
        self._colours: bool | None = None
        self._say_ignored(ignored)

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
        fields = encode_fields(attrs, self._redact, self._replace)
        if self._source:
            where = {"function": record.funcName, "file": record.pathname, "line": record.lineno}
            fields.append((SOURCE_KEY, to_json(where)))
        line = Line(
            at=int(record.created * 1e9),
            stream=self._stream,
            level=(record.levelno - 20) * 4 // 10,
            msg=record.getMessage(),
            context=encode_fields([("logger", record.name), *shared.get()], self._redact, self._replace),
            attrs=fields,
        )
        trace = carried.get()
        if trace is not None:
            line.trace_id = trace[0].hex()
            line.span_id = None if trace[1] is None else trace[1].hex()
        return line

    def _say_ignored(self, ignored: list[_Unread]) -> None:
        for name, value, expected in ignored:
            if self._console == "off" or f"{name}={value}" in _said_ignored:
                continue
            _said_ignored.add(f"{name}={value}")
            fields = encode_fields([("value", value), ("expected", expected)], None)
            self._print(Line(at=time_ns(), stream="tinystore", level=4, msg=f"{name} is ignored", attrs=fields))

    def _print(self, line: Line) -> None:
        if self._console == "off":
            return
        out = self._to if self._to is not None else sys.stderr
        terminal = out.isatty()
        # FORCE_COLOR other than 0 says a person reads a pipe, as an IDE's run console is one
        forced = _forced()
        if self._console == "json" or (self._console is None and not (terminal or forced)):
            out.write(json_line(line))
        else:
            colour = (terminal or forced) and self._colour(out, forced)
            out.write(pretty_line(line, colour, time=self._time, hide_stream=self._hide_stream))
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
    time: ConsoleTime | None = None,
    hide_stream: bool = False,
    to: TextIO | None = None,
    redact: Iterable[str] = (),
    keep_url_passwords: bool = False,
    replace: Callable[[str, Any], Any] | None = None,
    env: FromEnv | bool = True,
    source: bool = False,
) -> ConsoleHandler:
    """A logging.Handler of the console alone, for a program that wants the logger and not the records.

        logging.basicConfig(handlers=[tinystore.handler("app", redact=tinystore.SECRETS)], level=logging.INFO)

    store.records.handler(stream) in its place keeps every line too.
    """
    return ConsoleHandler(
        stream,
        level,
        console=console,
        time=time,
        hide_stream=hide_stream,
        to=to,
        redact=redact,
        keep_url_passwords=keep_url_passwords,
        replace=replace,
        env=env,
        source=source,
    )
