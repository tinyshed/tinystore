"""A test's clock: the time a private server opened with clock runs on, which a test moves forward."""

from __future__ import annotations

from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any

from ._time import Duration, ms, unix_ms
from ._wire.messages import METHODS
from ._wire.messages import Clock as ClockMessage

if TYPE_CHECKING:
    from ._connection import Connection, Link


class Clock:
    """The clock of a store opened with private=True and a clock.

    Moving it forward expires keys, makes jobs due and ages records at once,
    as Go's tinystore.Options.Clock does; a store on the system's time refuses
    it with InvalidError::

        async with tinystore.open(tmp_path, private=True, clock=datetime(2026, 10, 3, 9, tzinfo=UTC)) as store:
            await store.clock.advance("1h")
    """

    def __init__(self, link: Link) -> None:
        self._link = link

    async def now(self) -> datetime:
        """The time the server's clock reads."""
        return await self._move()

    async def advance(self, by: Duration) -> datetime:
        """Moves the clock forward by a while, and gives the time it then reads."""
        return await self._move(advance=ms(by))

    async def set(self, to: datetime) -> datetime:
        """Sets the clock to a time, which may not be before its own, and gives it."""
        return await self._move(at=unix_ms(to))

    async def _move(self, **fields: Any) -> datetime:
        async def attempt(connection: Connection) -> bytes:
            return await connection.session.call(METHODS["server.clock"], ClockMessage.encode(**fields))

        at = ClockMessage.decode(await self._link.run("write", attempt)).get("at", 0)
        return datetime.fromtimestamp(at / 1000, UTC)
