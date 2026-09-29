"""Durations and moments as the wire carries them: milliseconds."""

from __future__ import annotations

import math
from datetime import UTC, datetime, timedelta

from .errors import InvalidError

Duration = timedelta | int | float
"""A length of time: a timedelta, or seconds."""


def ms(d: Duration) -> int:
    """A duration's milliseconds."""
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
