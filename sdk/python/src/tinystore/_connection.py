"""A connection is a transport and the session over it; a link gives the store its connection.

It dials again once the last one ended: the directory's sidecar, found
through SERVE or started, a private child, or a remote server.
"""

from __future__ import annotations

import asyncio
import base64
import json
import secrets
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Literal

from . import _runtime
from ._session import LostError, Session, Stream
from ._wire.messages import Handle
from .errors import ClosedError, InvalidError, OutcomeUnknownError, UnavailableError

if TYPE_CHECKING:
    import os
    import ssl
    from collections.abc import Awaitable, Callable

    from ._wire.codec import Key

HANDSHAKE_TIME = 5.0
WINNER_TIME = 5.0
STARTS = 3


class Connection:
    def __init__(
        self,
        session: Session,
        transport: _runtime.Transport,
        closing: Callable[[], Awaitable[None]] | None = None,
    ) -> None:
        self.session = session
        self._transport = transport
        self._closing = closing
        self.handles: dict[tuple[int, bytes], asyncio.Future[bytes]] = {}

    async def handshake(self) -> None:
        try:
            await asyncio.wait_for(asyncio.shield(self.session.welcomed), HANDSHAKE_TIME)
        except BaseException:
            self._transport.close()
            self.session.end(ClosedError("no WELCOME within the handshake's time"))
            raise
        self.session.on_end(lambda _: self._transport.close())

    async def close(self) -> None:
        """Ends the connection; a private child is waited for until it has exited."""
        self.session.end(ClosedError("the store closed"))
        self._transport.close()
        if self._closing is not None:
            await self._closing()


async def dial(
    endpoint: str,
    *,
    token: str | None = None,
    secret: bytes | None = None,
    tls: ssl.SSLContext | None = None,
) -> Connection:
    """Connects, shakes hands and gives up after the handshake's time."""
    holder: list[Session] = []
    early: list[bytes] = []

    def data(chunk: bytes) -> None:
        if holder:
            holder[0].receive(chunk)
        else:
            early.append(bytes(chunk))

    try:
        transport = await _runtime.connect(endpoint, data, lambda err: holder[0].end(err) if holder else None, tls)
    except OSError as err:
        raise ClosedError(f"cannot reach {endpoint}: {err}") from err
    challenge = secrets.token_bytes(16) if secret is not None else None
    session = Session(transport.write, _runtime.CLIENT, token=token, secret=secret, challenge=challenge)
    holder.append(session)
    for chunk in early:
        session.receive(chunk)
    connection = Connection(session, transport)
    await connection.handshake()
    return connection


def sidecar(
    directory: str | os.PathLike[str], binary: Callable[[], str], idle: float | None = None
) -> Callable[[], Awaitable[Connection]]:
    """Finds the directory's sidecar through SERVE or starts one, as docs/server.md "SERVE" says.

    A sidecar found proves itself before the first call; one started exits
    with 3 when another holds the directory, whose SERVE the client then
    waits for.
    """
    absolute = Path(directory).resolve()
    serve = absolute / "server" / "SERVE"
    log = absolute / "server" / "serve.log"
    idling = [] if idle is None else ["--idle", f"{round(idle * 1000)}ms"]

    async def reach() -> Connection:
        found = await _reach_serve(serve)
        if found is not None:
            return found
        why = ""
        for _ in range(STARTS):
            child = _runtime.spawn_detached(
                [binary(), "serve", "--dir", str(absolute), "--local", "--log", str(log), *idling]
            )
            deadline = asyncio.get_running_loop().time() + WINNER_TIME
            pause = 0.005
            while asyncio.get_running_loop().time() < deadline:
                reached = await _reach_serve(serve)
                if reached is not None:
                    return reached
                code = child.poll()
                if code is not None and code != 3:
                    raise ClosedError(f"the sidecar exited with {code}: {_tail(log)}")
                await asyncio.sleep(pause)
                pause = min(pause * 2, 0.1)
            why = (
                "another process holds the directory but no sidecar answers"
                if child.poll() == 3
                else "no sidecar answered"
            )
        raise ClosedError(f"{why} in {STARTS * WINNER_TIME:.0f} s: {_tail(log)}")

    return reach


async def _reach_serve(serve: Path) -> Connection | None:
    """The sidecar SERVE names, once its proof checks; None on any failure."""
    try:
        published = json.loads(await asyncio.to_thread(serve.read_bytes))
        endpoint = published["endpoints"][0]
        secret = base64.urlsafe_b64decode(published["secret"] + "==")
    except (OSError, ValueError, KeyError, IndexError, TypeError):
        return None
    if published.get("protocol") != 1 or len(secret) != 32:
        return None
    try:
        return await dial(endpoint, secret=secret)
    except Exception:
        return None


def _tail(log: Path) -> str:
    try:
        return "\n".join(log.read_text(encoding="utf-8", errors="replace").strip().splitlines()[-5:])
    except OSError:
        return "it wrote no log"


def private_child(directory: str | os.PathLike[str], binary: Callable[[], str]) -> Callable[[], Awaitable[Connection]]:
    """A private child: tinystore serve --stdio, living and dying with this process."""

    absolute = str(Path(directory).resolve())

    async def start() -> Connection:
        holder: list[Session] = []
        child = await _runtime.spawn_private(
            [binary(), "serve", "--dir", absolute, "--stdio"],
            lambda chunk: holder[0].receive(chunk) if holder else None,
            lambda err: holder[0].end(err) if holder else None,
        )
        session = Session(child.transport.write, _runtime.CLIENT)
        holder.append(session)

        async def closing() -> None:
            # a child told to leave by the end of its stdin drains and exits;
            # one that does not is killed
            child.transport.close()
            try:
                await asyncio.wait_for(child.process.wait(), 10)
            except TimeoutError:
                child.process.kill()
                await child.process.wait()
            await child.reading

        session.on_end(lambda _: child.transport.close())
        connection = Connection(session, child.transport, closing)
        try:
            await connection.handshake()
        except BaseException as err:
            child.process.kill()
            raise ClosedError(f"the private server did not start: {err} {child.tail()}") from err
        return connection

    return start


def remote(url: str, token: str, tls: ssl.SSLContext | None) -> Callable[[], Awaitable[Connection]]:
    """A remote server: tls:// checks its certificate before the token leaves; tcp:// does not."""
    return lambda: dial(url, token=token, tls=tls)


class Link:
    """Gives calls a connection, dialling again once the last one ended.

    A call whose REQUEST had not left goes again when its connection is lost,
    so does a read that had, and a write that had is OutcomeUnknownError.
    """

    def __init__(self, dialer: Callable[[], Awaitable[Connection]]) -> None:
        self._dialer = dialer
        self._current: asyncio.Future[Connection] | None = None
        self._closed = False

    async def connection(self) -> Connection:
        if self._closed:
            raise ClosedError("the store is closed")
        current = self._current
        if current is not None:
            try:
                found = await asyncio.shield(current)
                if found.session.ended is None and not found.session.going_away:
                    return found
            except Exception:
                pass
            if self._current is not current:
                return await self.connection()
        dialled = asyncio.ensure_future(self._dialer())
        self._current = dialled
        try:
            return await asyncio.shield(dialled)
        except BaseException:
            if self._current is dialled:
                self._current = None
            raise

    async def run[T](self, idempotence: Literal["read", "write"], attempt: Callable[[Connection], Awaitable[T]]) -> T:
        for tries in range(3):
            connection = await self.connection()
            try:
                return await attempt(connection)
            except (LostError, UnavailableError) as err:
                sent = isinstance(err, LostError) and err.sent
                if tries < 2 and (not sent or idempotence == "read"):
                    continue
                if sent:
                    raise OutcomeUnknownError(
                        f"the connection was lost with the write in flight;"
                        f" read what it wrote before writing again: {err}"
                    ) from err
                raise
        raise ClosedError("unreachable")

    async def close(self) -> None:
        self._closed = True
        current, self._current = self._current, None
        if current is not None and current.done() and not current.cancelled() and current.exception() is None:
            await current.result().close()


def owner_text(owner: Key) -> str | bytes:
    """An owner of a branch as its text: an integer is its decimal spelling, as Go's Of takes it."""
    if isinstance(owner, bool):
        raise InvalidError(f"the owner {owner!r}: an owner is text or an integer")
    return str(owner) if isinstance(owner, int) else owner


async def handle_on(connection: Connection, method: int, open_body: bytes) -> int:
    """The handle an open answers on a connection, opened once a connection and tried again when it failed."""
    key = (method, open_body)
    opening = connection.handles.get(key)
    if opening is None:
        opening = asyncio.ensure_future(connection.session.call(method, open_body))
        connection.handles[key] = opening
    try:
        body = await asyncio.shield(opening)
    except BaseException:
        connection.handles.pop(key, None)
        raise
    return Handle.decode(body)["handle"]


@dataclass
class Downloaded:
    header: bytes
    items: list[bytes]
    trailer: bytes


async def download(connection: Connection, method: int, body: bytes) -> Downloaded:
    """Runs a download to its end, giving each item's credit back as it arrives."""
    stream: Stream = await connection.session.open(method, body, True)
    try:
        head = await stream.next()
        if head.end:
            return Downloaded(head.body, [], head.body)
        items: list[bytes] = []
        while True:
            event = await stream.next()
            stream.consumed(len(event.body))
            if event.end:
                return Downloaded(head.body, items, event.body)
            items.append(event.body)
    except asyncio.CancelledError:
        stream.cancel()
        raise


def check_name(name: str, of: str) -> None:
    import re

    if not re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,63}", name):
        raise InvalidError(f"the {of} name {name!r}: a name is [a-z0-9][a-z0-9_-]{{0,63}}")
