"""The store in a directory, as another process serves it."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Literal, Self

from ._connection import Link, private_child, remote, sidecar
from ._runtime import find_binary
from ._time import Duration, ms
from .blobs import Blobs
from .jobs import Jobs
from .kv import Kv
from .metrics import Metrics
from .records import Records
from .sql import Database, Migrations, open_database

if TYPE_CHECKING:
    import os
    import ssl
    from collections.abc import Awaitable, Callable, Generator

    from ._connection import Connection


@dataclass(frozen=True, slots=True)
class Status:
    """What a server is: its version, v0.1.0 or (devel), the protocol, its engines, the connection's capability."""

    server: str
    protocol: int
    engines: tuple[str, ...]
    capability: Literal["admin", "data"]


class Store:
    """The store: its engines, and one close. Opened by open or connect."""

    def __init__(self, link: Link) -> None:
        self._link = link
        self.kv = Kv(link)
        self.jobs = Jobs(link)
        self.blobs = Blobs(link)
        self.records = Records(link)
        self.metrics = Metrics(link)

    async def status(self) -> Status:
        """What the server this store reaches is, as its WELCOME said it.

        Its version, the protocol the connection speaks, the engines it serves
        and what the connection may do. A client newer than its server learns
        here what it may ask for; a call past it is UnimplementedError, naming
        the server's version.
        """
        connection = await self._link.connection()
        agreed = await connection.session.welcomed
        capability: Literal["admin", "data"] = "admin" if agreed.capability == "admin" else "data"
        return Status(agreed.server, agreed.protocol, tuple(agreed.engines), capability)

    async def sql(self, name: str, *, migrations: Migrations | None = None) -> Database:
        """A database of the application's own, sql/<name>.db.

        The first open in the server, on an admin connection, applies its
        migrations; every later one checks them against what the file applied.
        """
        return await open_database(self._link, name, migrations)

    async def close(self) -> None:
        """Ingests the instruments' last values, then closes the connection."""
        await self.metrics.stop()
        await self.records.stop()
        await self._link.close()

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(self, *_: object) -> None:
        await self.close()


class Opening:
    """A store being opened: await it, or enter it with async with to close it on the way out."""

    def __init__(self, dialer: Callable[[], Awaitable[Connection]]) -> None:
        self._dialer = dialer
        self._store: Store | None = None

    async def _open(self) -> Store:
        link = Link(self._dialer)
        await link.connection()
        self._store = Store(link)
        return self._store

    def __await__(self) -> Generator[Any, None, Store]:
        return self._open().__await__()

    async def __aenter__(self) -> Store:
        return await self._open()

    async def __aexit__(self, *_: object) -> None:
        if self._store is not None:
            await self._store.close()


def open(
    directory: str | os.PathLike[str],
    *,
    private: bool = False,
    binary: str | None = None,
    idle: Duration | None = None,
) -> Opening:
    """Opens the store in a directory through its sidecar, found through SERVE or started.

    private asks for a child of this process's own on stdin and stdout
    instead, which lives and dies with it. idle is how long a sidecar this
    process starts stays once its last connection has gone: 30 s unless
    given, 0 for ever. It returns once the server has answered.
    """

    def which() -> str:
        return find_binary(binary)

    if private:
        return Opening(private_child(directory, which))
    return Opening(sidecar(directory, which, None if idle is None else ms(idle) / 1000))


def connect(url: str, *, token: str, tls: ssl.SSLContext | None = None) -> Opening:
    """Connects to a remote server: tls:// checks its certificate before the token leaves."""
    return Opening(remote(url, token, tls))
