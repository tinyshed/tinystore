"""How a handle reaches the store: its open, made once a connection, and the calls on it.

A handle's calls go on whichever connection the link has, the handle opened
there first, or inside a database's transaction, on its stream; ``Via`` is
which.
"""

from __future__ import annotations

import asyncio
import weakref
from typing import TYPE_CHECKING, Protocol

from ._wire.protocol import Handle
from .errors import InvalidError

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

    from ._connection import Connection, Idempotence, Link
    from ._wire.codec import Key

type Opener = bytes | Callable[[Connection], Awaitable[bytes]]
"""What opens a handle: its open's bytes, or, for a handle kept in a database's
file, what makes them once the database is open on the connection, since the
open names the database by its handle there."""


class Home(Protocol):
    """A database a bucket or a queue is kept in: its handle on a connection, and its name as errors say it."""

    @property
    def describe(self) -> str: ...

    async def handle(self, connection: Connection) -> int: ...

    def keep(self, method: int, body: bytes, make: Callable[[Connection], Awaitable[bytes]]) -> Opener:
        """The opener of a handle kept in the database's file: the one made first for the same open.

        So that the database's transaction opens each once, before it holds
        the writer, and knows the handles it opened.
        """
        ...

    def via(self, what: str) -> Via:
        """How a handle kept in the database's file reaches the store outside a transaction.

        Refused from inside the database's own transaction, where it would
        wait for the writer the transaction holds.
        """
        ...


class Via(Protocol):
    """How a handle's calls reach the store."""

    async def call(
        self,
        open_method: int,
        opener: Opener,
        method: int,
        body: Callable[[int], bytes],
        idempotence: Idempotence,
    ) -> bytes: ...


class _ViaLink:
    def __init__(self, link: Link) -> None:
        self._link = link

    async def call(
        self,
        open_method: int,
        opener: Opener,
        method: int,
        body: Callable[[int], bytes],
        idempotence: Idempotence,
    ) -> bytes:
        async def attempt(connection: Connection) -> bytes:
            handle = await handle_on(connection, open_method, opener)
            return await connection.session.call(method, body(handle))

        return await self._link.run(idempotence, attempt)


def via_link(link: Link) -> Via:
    """Calls on whichever connection the link has, dialled again as it says."""
    return _ViaLink(link)


# what an opener of a database's file opened, by connection
_opened: weakref.WeakKeyDictionary[object, weakref.WeakKeyDictionary[Connection, asyncio.Future[int]]] = (
    weakref.WeakKeyDictionary()
)


async def handle_on(connection: Connection, method: int, opener: Opener) -> int:
    """The handle an open answers on a connection, opened once a connection.

    The open's own bytes name it, so a bucket opened twice with the same
    options shares its handle. An open that fails is tried again by the next
    call.
    """
    if isinstance(opener, bytes):
        return await _opened_by_bytes(connection, method, opener)
    by_connection: weakref.WeakKeyDictionary[Connection, asyncio.Future[int]] | None = _opened.get(opener)
    if by_connection is None:
        by_connection = weakref.WeakKeyDictionary()
        _opened[opener] = by_connection
    opening = by_connection.get(connection)
    if opening is None:

        async def made() -> int:
            return await _opened_by_bytes(connection, method, await opener(connection))

        opening = asyncio.ensure_future(made())
        by_connection[connection] = opening
    try:
        return await asyncio.shield(opening)
    except BaseException:
        if by_connection.get(connection) is opening:
            del by_connection[connection]
        raise


async def _opened_by_bytes(connection: Connection, method: int, body: bytes) -> int:
    key = (method, body)
    opening = connection.handles.get(key)
    if opening is None:
        opening = asyncio.ensure_future(connection.session.call(method, body))
        connection.handles[key] = opening
    try:
        answer = await asyncio.shield(opening)
    except BaseException:
        if connection.handles.get(key) is opening:
            del connection.handles[key]
        raise
    return Handle.decode(answer).get("handle", 0)


def key_text(key: Key) -> str:
    """A key or an owner as the store spells it: an integer its decimal text, bytes the text they spell."""
    if isinstance(key, bool):
        raise InvalidError(f"the key {key!r}: a key is text or an integer")
    if isinstance(key, int):
        return str(key)
    if isinstance(key, bytes):
        try:
            return key.decode()
        except UnicodeDecodeError:
            raise InvalidError("a key of bytes that are not UTF-8, which kv does not keep yet") from None
    if not isinstance(key, str):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"the key {key!r}: a key is text or an integer")
    return key
