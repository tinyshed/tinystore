"""A function run once a key, its answer kept.

A request sent twice is answered as the first was rather than done twice. The
core lets one run of a key go at a time, every client of the store included:
it answers the kept answer, or hands the run to this client and keeps what it
sends back::

    charges = store.once("charges", Receipt, keep="1d")
    receipt = await charges.run(request_id, lambda: pay.charge(order, request_id))
"""

from __future__ import annotations

import contextlib
import inspect
from typing import TYPE_CHECKING, Any, cast

from ._connection import check_name
from ._handles import handle_on
from ._session import ConnectionLostError, Stream
from ._time import Duration, ms
from ._wire.protocol import KvAnswer, KvCall, KvFound, KvOnceOpen, methods
from .errors import OutcomeUnknownError, ProtocolError
from .kv import Parts, below, call_on, under_field

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

    from ._connection import Connection
    from ._values import Values
    from ._wire.codec import Key


def once_open(name: str, keep: Duration | None) -> bytes:
    check_name(name, "once")
    return KvOnceOpen.encode(name=name, keep=None if keep is None else ms(keep, f"kv once {name}: keep"))


class Once[T]:
    """Answers kept once a key: open them with ``store.once(name, type)``."""

    def __init__(self, parts: Parts) -> None:
        self.name = parts.name
        self._parts = parts
        assert parts.values is not None
        self._values: Values[T] = parts.values

    def under(self, *owners: Key) -> Once[T]:
        """The answers of a branch, each key apart from the same key elsewhere."""
        return Once(below(self._parts, owners))

    async def run(self, key: Key, fn: Callable[[], Awaitable[T] | T]) -> T:
        """The answer kept for key, or fn's, which is kept.

        A run of a key that another runs, here or in another process, waits for
        it and gets its answer; an exception of fn keeps nothing and is raised,
        so the next run runs fn again. A connection lost after fn returned and
        before its answer was kept is OutcomeUnknownError.
        """
        handed = await self._claim(key)
        if not isinstance(handed, Stream):
            return handed[0]
        try:
            answered = fn()
            answer = cast("T", await answered) if inspect.isawaitable(answered) else answered
            kept = KvAnswer.encode(found=True, value=self._values.encode(answer))
        except BaseException:
            # fn's own error is the one to raise
            with contextlib.suppress(Exception):
                await _end(handed, KvAnswer.encode())
            raise
        await _end(handed, kept)
        return answer

    async def get(self, key: Key) -> T | None:
        """The answer kept for key, None when none is."""
        kept = KvAnswer.decode(await call_on(self._parts, "kv.once.get", {"key": key}, "read"))
        return self._values.decode(kept.get("value")) if kept.get("found") else None

    async def delete(self, key: Key) -> bool:
        """Forgets the answer kept for key, so that its next run runs fn again."""
        return KvFound.decode(await call_on(self._parts, "kv.once.delete", {"key": key}, "write")).get("found", False)

    async def _claim(self, key: Key) -> Stream | tuple[T]:
        # asks for key's run: the answer kept for it, or the run the core handed
        # over, which a connection lost before the answer asks for again
        async def attempt(connection: Connection) -> Stream | tuple[T]:
            handle = await handle_on(connection, methods["kv.once.open"], self._parts.opener)
            call = KvCall.encode(handle=handle, under=under_field(self._parts.under), key=key)
            stream = await connection.session.open(methods["kv.once.run"], call, False)
            try:
                first = await stream.next()
                offered: dict[str, Any] = KvAnswer.decode(first.body)
                if not first.end:
                    return stream
                if not offered.get("found"):
                    raise ProtocolError(f"kv once {self.name}: a run ended without its answer")
                return (self._values.decode(offered.get("value")),)
            except BaseException:
                stream.cancel()
                raise

        return await self._parts.link.run("read", attempt)


async def _end(stream: Stream, answer: bytes) -> None:
    """Sends the run's answer as the stream's last DATA, and waits for the core's, once it is kept."""
    try:
        await stream.send(answer, True)
        await stream.next()
    except ConnectionLostError as err:
        raise OutcomeUnknownError(
            f"the connection was lost before the answer was kept; the next run may run again: {err}"
        ) from err
