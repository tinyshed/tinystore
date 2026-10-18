"""What the SDK does in the background, a worker's loop or an instrument's flush.

It has no caller to tell when it fails, so it says so in TinyStore's own
lines: once, again when the failure changes or ten minutes on, and once more
when the work recovers.
"""

from __future__ import annotations

import json
import sys
import time
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any, Literal

if TYPE_CHECKING:
    from collections.abc import Callable

_QUIET = 600.0
"""A failure repeated within this many seconds is counted rather than said again."""

type Level = Literal["info", "warn"]
type Say = Callable[[Level, str, dict[str, Any]], None]
"""Where TinyStore's own lines go: the console's stream tinystore, or a test's."""


def say_own(level: Level, message: str, fields: dict[str, Any]) -> None:
    """Says one of TinyStore's own lines on stderr, as a JSON line of the stream tinystore."""
    stamp = datetime.now(UTC).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    line = {"time": stamp, "level": level.upper(), "stream": "tinystore", "msg": message, **fields}
    print(json.dumps(line, ensure_ascii=False, default=str), file=sys.stderr, flush=True)


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
