"""Spans and moments as the wire carries them: milliseconds.

A span is text, ``"30d"``, ``"1h30m"``, ``"15m"``, ``"1s"`` or ``"250ms"``, or
a ``timedelta``; a moment is an aware ``datetime``. A bare number is refused,
so that ``30`` is never thirty milliseconds to one reader and thirty seconds
to another.
"""

from __future__ import annotations

import re
from datetime import UTC, datetime, timedelta

from .errors import InvalidError

Duration = timedelta | str
"""A length of time: a timedelta, or its text, such as ``"30d"``, ``"1h30m"``, ``"15m"``, ``"1s"`` and ``"250ms"``."""

_UNITS = {"w": 604_800_000, "d": 86_400_000, "h": 3_600_000, "m": 60_000, "s": 1000, "ms": 1}
_SPELLED = re.compile(r"(?:(\d+)w)?(?:(\d+)d)?(?:(\d+)h)?(?:(\d+)m(?!s))?(?:(\d+)s)?(?:(\d+)ms)?")


def ms(d: Duration, of: str = "a duration") -> int:
    """A span's milliseconds; spelled, each unit once and largest first: 1h30m, not 90m30m."""
    if isinstance(d, str):
        text = d.strip()
        parts = _SPELLED.fullmatch(text)
        if parts is None or not text:
            raise InvalidError(f"{of} {d!r}: write it as 30d, 1h30m, 15m, 1s or 250ms")
        total = sum(int(n) * _UNITS[unit] for n, unit in zip(parts.groups(), _UNITS, strict=True) if n is not None)
        if total >= 1 << 63:
            raise InvalidError(f"{of} {d!r}: past what 64 bits of milliseconds hold")
        return total
    if isinstance(d, timedelta):  # pyright: ignore[reportUnnecessaryIsInstance]
        if d < timedelta(0):
            raise InvalidError(f"{of} of {d}, before nothing")
        return d // timedelta(milliseconds=1)
    raise InvalidError(f"{of} {d!r}: a span is a timedelta or its text, such as '30s', never a bare number")


def spelled(milliseconds: int) -> str:
    """Milliseconds as a span is written, largest units first: 5_400_000 is 1h30m, 0 is 0s."""
    if milliseconds == 0:
        return "0s"
    left, text = milliseconds, ""
    for unit, size in _UNITS.items():
        if left >= size:
            text += f"{left // size}{unit}"
            left %= size
    return text


def spelled_keep(milliseconds: int) -> str:
    """A keep as a span is written, in its largest unit that divides it: 30d, 12h, 90s, 250ms."""
    for unit, size in (("d", 86_400_000), ("h", 3_600_000), ("m", 60_000), ("s", 1000)):
        if milliseconds >= size and milliseconds % size == 0:
            return f"{milliseconds // size}{unit}"
    return "0s" if milliseconds == 0 else f"{milliseconds}ms"


def unix_ms(t: datetime, of: str = "a time") -> int:
    """A moment's unix milliseconds; a naive datetime is refused, its zone being anyone's guess."""
    if not isinstance(t, datetime):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"{of} {t!r}: a time is an aware datetime")
    if t.tzinfo is None:
        raise InvalidError(f"{of} {t.isoformat()} has no zone: give it one, as datetime.now(UTC) does")
    return (t - _EPOCH) // timedelta(milliseconds=1)


_EPOCH = datetime(1970, 1, 1, tzinfo=UTC)


def date_of(unix: int | None) -> datetime | None:
    """A moment the server named in unix milliseconds, none for 0 or absent."""
    if unix is None or unix == 0:
        return None
    return _EPOCH + timedelta(milliseconds=unix)


def moment(unix: int) -> datetime:
    """A moment the server named in unix milliseconds, 0 the epoch itself."""
    return _EPOCH + timedelta(milliseconds=unix)
