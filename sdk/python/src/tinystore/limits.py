"""May this request pass.

A rate limit lets a smooth rate of requests a key through, the generic cell
rate algorithm with its times in the core's memory; a quota counts uses in
windows that reset, all of them or none in one durable write. Both answer
rather than raise, with one Allowance::

    api = store.rate_limit("api", rate="100/s", burst=20)
    answer = await api.allow(user_id)                       # answer.ok, answer.left, answer.retry_at
    ai = store.quota("ai", daily="100/1d", weekly="300/7d")
    answer = await ai.allow(user_id)                        # answer.windows["weekly"].left
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from ._connection import check_name
from ._time import date_of, ms
from ._wire.protocol import KvAllowance, KvQuotaOpen, KvRateLimitOpen
from .errors import InvalidError
from .kv import Parts, below, call_on

if TYPE_CHECKING:
    from datetime import datetime

    from ._connection import Idempotence
    from ._wire.codec import Key


@dataclass(frozen=True, slots=True)
class WindowUse:
    """One window of a quota as a key stands in it."""

    used: int
    limit: int
    left: int
    resets_at: datetime | None
    """when the window resets; None for one not started"""


@dataclass(frozen=True, slots=True)
class Allowance:
    """What a rate limit or a quota answers a request."""

    ok: bool
    left: int
    """how many more would pass now"""
    retry_at: datetime | None
    """when a refused request may try again; None when it passed"""
    windows: dict[str, WindowUse] = field(default_factory=dict[str, WindowUse])
    """a quota's windows by name; none for a rate limit"""


_RATE = re.compile(r"(\d+)/(\d*)([a-z]+)")


def rate_of(rate: str) -> tuple[int, int]:
    """A rate's count and span in milliseconds: "100/s" is 100 a 1000 ms, "5/10s" 5 a 10000."""
    parts = _RATE.fullmatch(rate) if isinstance(rate, str) else None  # pyright: ignore[reportUnnecessaryIsInstance]
    if parts is None:
        raise InvalidError(f"the rate {rate!r}; write it as 100/s, 5/10s or 1000/h")
    return int(parts[1]), ms(f"{parts[2] or '1'}{parts[3]}", f"the rate {rate!r}")


def rate_limit_open(name: str, rate: str, burst: int | None) -> bytes:
    check_name(name, "rate limit")
    count, per = rate_of(rate)
    if count < 1 or per < 1:
        raise InvalidError(f"kv rate limit {name}: a rate of {rate}")
    if burst is not None and (isinstance(burst, bool) or not isinstance(burst, int) or burst < 1):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"kv rate limit {name}: a burst of {burst!r}")
    return KvRateLimitOpen.encode(name=name, rate=count, per=per, burst=burst)


def quota_open(name: str, windows: dict[str, str]) -> bytes:
    """A quota's open: its windows by name, in the order they were given."""
    check_name(name, "quota")
    named: list[dict[str, Any]] = []
    for window, rate in windows.items():
        count, per = rate_of(rate)
        named.append({"name": window, "limit": count, "per": per})
    return KvQuotaOpen.encode(name=name, windows=named)


def _allowance_of(body: bytes) -> Allowance:
    answer = KvAllowance.decode(body)
    windows = {
        window.get("name", ""): WindowUse(
            used=window.get("used", 0),
            limit=window.get("limit", 0),
            left=window.get("left", 0),
            resets_at=date_of(window.get("resets_at")),
        )
        for window in answer.get("windows", [])
    }
    return Allowance(
        ok=answer.get("ok", False),
        left=answer.get("left", 0),
        retry_at=date_of(answer.get("retry_at")),
        windows=windows,
    )


class RateLimit:
    """A rate of requests a key: open it with ``store.rate_limit(name, rate=...)``."""

    def __init__(self, parts: Parts) -> None:
        self.name = parts.name
        self._parts = parts

    def under(self, *owners: Key) -> RateLimit:
        """The rate limit of a branch, each key apart from the same key elsewhere."""
        return RateLimit(below(self._parts, owners))

    async def allow(self, key: Key, n: int = 1) -> Allowance:
        """Asks for n requests of key, one unless given: all pass or none; more than the burst is InvalidError."""
        return _allowance_of(await self._call("kv.allow", key, n, "write"))

    async def peek(self, key: Key) -> Allowance:
        """The answer one request would get, using nothing."""
        return _allowance_of(await self._call("kv.peek", key, None, "read"))

    async def reset(self, key: Key) -> None:
        """Forgets key, so that its next request starts afresh."""
        await self._call("kv.reset", key, None, "write")

    async def _call(self, method: str, key: Key, n: int | None, idempotence: Idempotence) -> bytes:
        return await call_on(self._parts, method, {"key": key, "n": None if n == 1 else n}, idempotence)


class Quota:
    """Uses by key in named windows: open it with ``store.quota(name, daily="100/1d")``."""

    def __init__(self, parts: Parts) -> None:
        self.name = parts.name
        self._parts = parts

    def under(self, *owners: Key) -> Quota:
        return Quota(below(self._parts, owners))

    async def allow(self, key: Key, n: int = 1) -> Allowance:
        """Uses n of key's every window, one unless given, or none when one has no room.

        More than a window's limit is InvalidError.
        """
        return _allowance_of(await self._call("kv.allow", key, n, "write"))

    async def peek(self, key: Key) -> Allowance:
        return _allowance_of(await self._call("kv.peek", key, None, "read"))

    async def reset(self, key: Key) -> None:
        await self._call("kv.reset", key, None, "write")

    async def refund(self, key: Key, n: int = 1) -> None:
        """Gives n uses back to every window of key, one unless given."""
        await self._call("kv.refund", key, n, "write")

    async def _call(self, method: str, key: Key, n: int | None, idempotence: Idempotence) -> bytes:
        return await call_on(self._parts, method, {"key": key, "n": None if n == 1 else n}, idempotence)
