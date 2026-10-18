"""What the SDK does in the background, a worker's loop or an instrument's flush.

It has no caller to tell when it fails, so it says so in TinyStore's own
lines: once, again when the failure changes or ten minutes on, and once more
when the work recovers.
"""

from __future__ import annotations

import functools
import time
from typing import TYPE_CHECKING, Any, Literal

from ._console import Line, Printer, encode_fields

if TYPE_CHECKING:
    from collections.abc import Callable

_QUIET = 600.0
"""A failure repeated within this many seconds is counted rather than said again."""

type Level = Literal["info", "warn"]
type Say = Callable[[Level, str, dict[str, Any]], None]
"""Where TinyStore's own lines go: the console's stream tinystore, or a test's."""


def say_own(level: Level, message: str, fields: dict[str, Any]) -> None:
    """Says one of TinyStore's own lines, as the console of a logger of the stream tinystore prints it."""
    attrs = encode_fields(fields.items(), None)
    _own().print(Line(at=time.time_ns(), stream="tinystore", level=_LEVELS[level], msg=message, attrs=attrs))


_LEVELS = {"info": 0, "warn": 4}


@functools.cache
def _own() -> Printer:
    return Printer(None, None, False, None)


def message_of(err: BaseException | str) -> str:
    return err if isinstance(err, str) else str(err) or type(err).__name__


class FailureLog:
    """A background work's failures, said as the core's are::

    00:00  disk full        → warn "background work failed" failures=1
    00:01…00:09 the same    → counted
    00:10  disk full        → warn failures=11
    00:11  success          → info "background work recovered" failures=11
    """

    def __init__(self, work: str, say: Say = say_own, now: Callable[[], float] = time.monotonic) -> None:
        self._work, self._say, self._now = work, say, now
        self._last = ""
        self._said_at = 0.0
        self._failures = 0

    def failed(self, err: BaseException) -> None:
        self._failures += 1
        error = message_of(err)
        if error == self._last and self._now() - self._said_at < _QUIET:
            return
        self._say("warn", "background work failed", {"work": self._work, "error": error, "failures": self._failures})
        self._last, self._said_at = error, self._now()

    def succeeded(self) -> None:
        if self._failures > 0:
            self._say("info", "background work recovered", {"work": self._work, "failures": self._failures})
        self._last, self._failures = "", 0
