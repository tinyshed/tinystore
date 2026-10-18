"""A test's clock: the time a private server opened with ``clock`` runs on.

A test moves it forward rather than waits for it.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from ._time import Duration, moment, ms, unix_ms
from ._wire.protocol import ServerClock, methods

if TYPE_CHECKING:
    from datetime import datetime

    from ._connection import Link


class Clock:
    """The clock of a store opened with ``private=True, clock=...``.

    Moving it forward expires keys, makes jobs due and ages records at once;
    a store on the system's time refuses it with InvalidError::

        store = await tinystore.open(dir, private=True, clock=datetime(2026, 10, 9, 9, tzinfo=UTC))
        await store.clock.advance("1h")
    """

    def __init__(self, link: Link) -> None:
        self._link = link

    async def now(self) -> datetime:
        """The time the server's clock reads."""
        return await self._move()

    async def advance(self, by: Duration) -> datetime:
        """Moves the clock forward by a while, and gives the time it then reads."""
        return await self._move(advance=ms(by, "a clock's advance"))

    async def set(self, to: datetime) -> datetime:
        """Sets the clock to a time, which may not be before its own, and gives it."""
        return await self._move(at=unix_ms(to, "a clock's time"))

    async def _move(self, *, advance: int | None = None, at: int | None = None) -> datetime:
        body = ServerClock.encode(advance=advance, at=at)
        answer = await self._link.run("write", lambda c: c.session.call(methods["server.clock"], body))
        return moment(ServerClock.decode(answer).get("at", 0))
