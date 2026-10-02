"""Durations and moments as the wire carries them: milliseconds."""

from __future__ import annotations

import math
import re
from datetime import UTC, datetime, timedelta

from .errors import InvalidError

Duration = timedelta | int | float | str
"""A length of time: a timedelta, seconds, or text such as "30d", "1h30m", "15m", "1s" and "250ms"."""

_UNITS = {"w": 604_800_000, "d": 86_400_000, "h": 3_600_000, "m": 60_000, "s": 1000, "ms": 1}
_SPELLED = re.compile(r"^(?:(\d+)w)?(?:(\d+)d)?(?:(\d+)h)?(?:(\d+)m(?!s))?(?:(\d+)s)?(?:(\d+)ms)?$")


def ms(d: Duration) -> int:
    """A duration's milliseconds; spelled, each unit once, largest first: 1h30m, not 90m30m."""
    if isinstance(d, str):
        parts = _SPELLED.match(d.strip())
        if parts is None or not d.strip():
            raise InvalidError(f"the duration {d!r}; write it as 30d, 1h30m, 15m, 1s or 250ms")
        return sum(int(n) * _UNITS[unit] for n, unit in zip(parts.groups(), _UNITS, strict=True) if n is not None)
    seconds = d.total_seconds() if isinstance(d, timedelta) else float(d)
    if not math.isfinite(seconds) or seconds < 0:
        raise InvalidError(f"a duration of {seconds} seconds")
    return round(seconds * 1000)


def unix_ms(t: datetime) -> int:
    """A moment's unix milliseconds; a naive one is local time, as datetime.timestamp takes it."""
    return math.floor(t.timestamp() * 1000)


def unix_ns(t: datetime | int) -> int:
    """A moment's unix nanoseconds: an int is nanoseconds already, a datetime its microseconds'."""
    if isinstance(t, int):
        return t
    whole = int(t.timestamp()) if t.tzinfo is not None else int(t.astimezone().timestamp())
    return whole * 1_000_000_000 + t.microsecond * 1_000


def date_of(unix: int | None) -> datetime | None:
    """A moment the server named in unix milliseconds, none for 0 or absent."""
    if unix is None or unix == 0:
        return None
    return datetime.fromtimestamp(unix / 1000, tz=UTC)
