"""A limiter of requests by key, as Go's kv.Limiter keeps one.

The generic cell rate algorithm: one time a key, which the server holds in memory and writes every
second, so a crash lets at most one burst more through.
"""

from __future__ import annotations

import re
from typing import TYPE_CHECKING, Any, NamedTuple

from ._connection import Connection, Link, check_name, handle_on, owner_text
from ._time import ms
from ._wire.messages import METHODS, KvAllowance, KvBucket, KvCall
from .errors import InvalidError

if TYPE_CHECKING:
    from ._wire.codec import Key


class Allowance(NamedTuple):
    """What a limiter answers a request."""

    ok: bool
    left: int
    """how many more requests would pass now"""
    retry_after: float
    """seconds until the request would pass; 0 when ok"""


def rate_of(rate: str) -> tuple[int, int]:
    """A rate's count and span in milliseconds; a span of one unit may leave out its 1.

    "100/s" → (100, 1000), "5/10s" → (5, 10000)
    """
    parts = re.fullmatch(r"(\d+)/(\d*)([a-z]+)", rate)
    if parts is None:
        raise InvalidError(f"the rate {rate!r}; write it as 100/s, 5/10s or 1000/h")
    count, span, unit = parts.groups()
    return int(count), ms(f"{span or '1'}{unit}")


class Limiter:
    """A limiter: open it with `store.kv.limiter(name, rate="100/s")`."""

    def __init__(self, link: Link, name: str, open_body: bytes, owners: tuple[str | bytes, ...]) -> None:
        self._link, self.name, self._open, self._owners = link, name, open_body, owners

    def of(self, *owners: Key) -> Limiter:
        """The limiter of a branch, each key apart from the same key elsewhere."""
        return Limiter(self._link, self.name, self._open, (*self._owners, *map(owner_text, owners)))

    async def allow(self, key: Key, n: int = 1) -> Allowance:
        """Asks for n requests of key: all pass or none does; past the burst is InvalidError."""

        async def attempt(connection: Connection) -> dict[str, Any]:
            handle = await handle_on(connection, METHODS["kv.open"], self._open)
            body = KvCall.encode(handle=handle, owners=list(self._owners) or None, key=key, n=None if n == 1 else n)
            return KvAllowance.decode(await connection.session.call(METHODS["kv.allow"], body))

        answer = await self._link.run("write", attempt)
        return Allowance(bool(answer.get("ok")), answer.get("left") or 0, (answer.get("retry_after") or 0) / 1000)


def limiter_open(name: str, rate: str, burst: int | None) -> bytes:
    """A limiter's open: its name and rate, checked before anything leaves."""
    check_name(name, "limiter")
    count, per = rate_of(rate)
    if count < 1 or per < 1:
        raise InvalidError(f"limiter {name}: a rate of {rate}")
    if burst is not None and burst < 1:
        raise InvalidError(f"limiter {name}: a burst of {burst}")
    return KvBucket.encode(name=name, rate=count, per=per, burst=burst)
