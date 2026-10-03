"""What the SDK does in the background, an instrument's flush or a handler's write.

It has no caller to tell when it fails, so it says so as Go's tinystore.Store
does, in TinyStore's own lines: once, again when the failure changes or ten
minutes on, and once more when the work recovers.
"""

from __future__ import annotations

import functools
import threading
import time
from typing import TYPE_CHECKING, Any, Literal

from ._console import ConsoleHandler, Line, encode_fields

if TYPE_CHECKING:
    from collections.abc import Callable

_QUIET = 600.0
"""A failure repeated within this many seconds is counted rather than said again."""

type Level = Literal["info", "warn"]
type Say = Callable[[Level, str, dict[str, Any]], None]
"""Where TinyStore's own lines go: the console of a handler of stream tinystore, or a test's."""

_LEVELS: dict[Level, int] = {"info": 0, "warn": 4}


class _Own(ConsoleHandler):
    """TinyStore's own lines, written as a handler of the console alone writes its own."""

    def say(self, level: Level, message: str, fields: dict[str, Any]) -> None:
        line = Line(
            at=time.time_ns(),
            stream="tinystore",
            level=_LEVELS[level],
            msg=message,
            attrs=encode_fields(fields.items(), None),
        )
        self._print(line)


@functools.cache
def _own() -> _Own:
    return _Own("tinystore")


def say_own(level: Level, message: str, fields: dict[str, Any]) -> None:
    """Says one of TinyStore's own lines, on stderr as the console of a handler of stream tinystore."""
    _own().say(level, message, fields)


class FailureLog:
    """A background work's failures, said as Go's failureLog logs them::

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
        error = str(err)
        if error == self._last and self._now() - self._said_at < _QUIET:
            return
        self._say("warn", "background work failed", {"work": self._work, "error": error, "failures": self._failures})
        self._last, self._said_at = error, self._now()

    def succeeded(self) -> None:
        if self._failures > 0:
            self._say("info", "background work recovered", {"work": self._work, "failures": self._failures})
        self._last, self._failures = "", 0


class DropNotice:
    """The lines a handler dropped for want of room, said at a flush and at most once in a quiet period.

    A burst is one line, and a handler that keeps dropping says how many every
    ten minutes rather than every second. A line is dropped on whichever
    thread logged it and said on the loop's, so the count has a lock of its own.
    """

    def __init__(self, logger: str, buffer: int, say: Say = say_own, now: Callable[[], float] = time.monotonic) -> None:
        self._logger, self._buffer, self._say, self._now = logger, buffer, say, now
        self._counting = threading.Lock()
        self._unsaid = 0
        self._said_at: float | None = None

    def dropped(self) -> None:
        with self._counting:
            self._unsaid += 1

    def say_if_due(self) -> None:
        with self._counting:
            if self._unsaid == 0 or (self._said_at is not None and self._now() - self._said_at < _QUIET):
                return
            dropped, self._unsaid, self._said_at = self._unsaid, 0, self._now()
        # logger, since a console line's own stream is tinystore's
        self._say("warn", "log lines dropped", {"logger": self._logger, "dropped": dropped, "buffer": self._buffer})
