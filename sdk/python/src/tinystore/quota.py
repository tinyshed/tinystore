"""A quota of uses by key, as Go's kv.Quota keeps one.

Several windows, each starting at a key's first use after the last ended, checked and counted
together in one durable write, or not counted at all.
"""

from __future__ import annotations

import re
from typing import TYPE_CHECKING, Any, NamedTuple

from ._connection import Connection, Link, check_name, handle_on, owner_text
from ._time import date_of
from ._wire.messages import METHODS, KvAllowance, KvBucket, KvCall
from .errors import InvalidError
from .limiter import rate_of

if TYPE_CHECKING:
    from datetime import datetime

    from ._wire.codec import Key


class WindowUsage(NamedTuple):
    """One window of a key: what it used of its limit, and when it resets."""

    used: int
    limit: int
    left: int
    reset_at: datetime | None
    """when the window ends and the next use starts another; None before it starts"""


class QuotaUsage(NamedTuple):
    """What a quota answers a key: what a limiter answers, and each window by its name."""

    ok: bool
    left: int
    """how many more uses would pass now"""
    retry_after: float
    """seconds until a refused use would pass; 0 when ok"""
    windows: dict[str, WindowUsage]


class Quota:
    """A quota: open it with `store.kv.quota(name, session="100/5h", weekly="300/7d")`."""

    def __init__(self, link: Link, name: str, open_body: bytes, owners: tuple[str | bytes, ...]) -> None:
        self._link, self.name, self._open, self._owners = link, name, open_body, owners

    def of(self, *owners: Key) -> Quota:
        """The quota of a branch, each key apart from the same key elsewhere."""
        return Quota(self._link, self.name, self._open, (*self._owners, *map(owner_text, owners)))

    async def allow(self, key: Key, n: int = 1) -> QuotaUsage:
        """Uses n of key's every window, or none when one has no room; past a window's limit is InvalidError."""
        return _usage(await self._call("kv.allow", "write", key, n))

    async def get(self, key: Key) -> QuotaUsage:
        """Key's windows without using them: ok says whether one more use would pass now."""
        return _usage(await self._call("kv.usage", "read", key, 1))

    async def refund(self, key: Key, n: int = 1) -> None:
        """Gives n uses back to each of key's windows that has not reset since."""
        await self._call("kv.refund", "write", key, n)

    async def delete(self, key: Key) -> None:
        """Forgets key's windows, so that its next use starts each anew."""
        await self._call("kv.delete", "write", key, 1)

    async def _call(self, method: str, idempotence: Any, key: Key, n: int) -> bytes:
        async def attempt(connection: Connection) -> bytes:
            handle = await handle_on(connection, METHODS["kv.open"], self._open)
            body = KvCall.encode(handle=handle, owners=list(self._owners) or None, key=key, n=None if n == 1 else n)
            return await connection.session.call(METHODS[method], body)

        return await self._link.run(idempotence, attempt)


def _usage(body: bytes) -> QuotaUsage:
    answer = KvAllowance.decode(body)
    windows = {
        w.get("name", ""): WindowUsage(
            w.get("used", 0), w.get("limit", 0), w.get("left", 0), date_of(w.get("reset_at"))
        )
        for w in answer.get("windows", [])
    }
    return QuotaUsage(bool(answer.get("ok")), answer.get("left", 0), answer.get("retry_after", 0) / 1000, windows)


def quota_open(name: str, windows: dict[str, str]) -> bytes:
    """A quota's open: its name and windows, checked before anything leaves."""
    check_name(name, "quota")
    if not 1 <= len(windows) <= 8:
        raise InvalidError(f"quota {name}: {len(windows)} windows, not one to eight")
    spans: list[dict[str, Any]] = []
    for window, rate in windows.items():
        if re.fullmatch(r"[a-z][a-z0-9_]{0,31}", window) is None:
            raise InvalidError(f"quota {name}: a window name is [a-z][a-z0-9_]{{0,31}}, not {window}")
        count, per = rate_of(rate)
        if count < 1 or per < 1:
            raise InvalidError(f"quota {name}: a window of {rate}")
        spans.append({"name": window, "limit": count, "per": per})
    return KvBucket.encode(name=name, windows=spans)
