"""The store in a directory, as another process serves it."""

from __future__ import annotations

import os
import re
import secrets
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any, Literal, Self

from ._connection import Link, private_child, remote, sidecar
from ._runtime import find_binary
from ._time import Duration, ms
from ._wire.messages import METHODS, Backup
from .blobs import Blobs
from .clock import Clock
from .errors import InvalidError
from .jobs import Jobs
from .kv import Kv
from .metrics import Metrics
from .records import Records
from .sql import Database, Migrations, open_database

if TYPE_CHECKING:
    import ssl
    from collections.abc import Awaitable, Callable, Generator, Iterable
    from datetime import datetime

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
        self.clock = Clock(link)
        """The test's clock of a private store opened with clock; on any other, its calls are InvalidError."""
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
        Without migrations it opens the file as it is, an empty one if there is
        none, and checks nothing.
        """
        return await open_database(self._link, name, migrations)

    async def backup(self, path: str | os.PathLike[str], *, files: Iterable[str] = ()) -> None:
        """Writes a backup of the whole store to a zip at path while the store keeps working, as tinystore backup does.

        The zip holds every engine's file with its size and checksum, which
        tinystore restore checks. It is written beside path and renamed into
        place once whole, so a backup that fails leaves no zip. It needs an
        admin connection; a remote server sends the zip over it. It holds no
        file but the engines' unless files names it: a file of the
        application's inside the store's directory, such as files=["secret.key"].
        """
        request = Backup.encode(files=list(files) or None)
        target = Path(path)
        part = target.with_name(f"{target.name}.{secrets.token_hex(4)}.part")

        async def attempt(connection: Connection) -> None:
            stream = await connection.session.open(METHODS["server.backup"], request, True)
            with part.open("wb") as file:
                await stream.next()  # the RESPONSE that heads the zip
                while True:
                    event = await stream.next()
                    stream.consumed(len(event.body))
                    file.write(event.body)
                    if event.end:
                        break
                file.flush()
                os.fsync(file.fileno())

        try:
            await self._link.run("read", attempt)
            part.replace(target)
        except BaseException:
            part.unlink(missing_ok=True)
            raise

    async def close(self) -> None:
        """Ingests the instruments' last values, then closes the connection."""
        await self.metrics.stop()
        await self.records.stop()
        self.kv.stop()
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
    clock: datetime | None = None,
) -> Opening:
    """Opens the store in a directory through its sidecar, found through SERVE or started.

    private asks for a child of this process's own on stdin and stdout
    instead, which lives and dies with it. idle is how long a sidecar this
    process starts stays once its last connection has gone: 30 s unless
    given, 0 for ever. clock runs a private server on a test's clock, from
    that time, which store.clock moves forward instead of a test waiting:
    keys expire, jobs come due and records age at once. It returns once the
    server has answered.
    """

    def which() -> str:
        return find_binary(binary)

    _check_directory(os.fspath(directory))
    if clock is not None and not private:
        raise InvalidError("a clock is a private server's: a shared sidecar runs on the system's time")
    if private:
        return Opening(private_child(directory, which, clock))
    return Opening(sidecar(directory, which, None if idle is None else ms(idle) / 1000))


# two letters or more before a colon: an address, never a Windows drive
_ADDRESS = re.compile(r"^([a-z][a-z0-9+.-]+):", re.IGNORECASE)


def _check_directory(directory: str) -> None:
    """Refuses an address where a directory belongs.

    open("tcp://…") would make a folder of that name and start a server in it,
    rather than reach the one meant:

        open("tcp://db.internal:7070")       → use connect
        open("pipe:tinystore-d761f24b7e59")  → open the directory it serves
        open("C:/data")                      → a directory
    """
    found = _ADDRESS.match(directory)
    if found is None:
        return
    if found.group(1).lower() in ("tcp", "tls"):
        raise InvalidError(
            f"open takes a store's directory, and {directory} is a server's address: "
            f"connect({directory!r}, token=...) reaches it"
        )
    raise InvalidError(
        f"open takes a store's directory, and {directory} is an address: "
        "open the directory its server serves, and it is found through SERVE"
    )


def connect(url: str, *, token: str, tls: ssl.SSLContext | None = None) -> Opening:
    """Connects to a remote server: tls:// checks its certificate before the token leaves."""
    return Opening(remote(url, token, tls))
