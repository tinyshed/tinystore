"""A connection is a transport and the session over it; a link gives the store its connection.

It dials again once the last one ended: the core in this process, the
directory's sidecar, found through SERVE or started, a private child, or a
remote server.
"""

from __future__ import annotations

import asyncio
import base64
import json
import re
import secrets
import sys
from dataclasses import dataclass
from datetime import UTC
from pathlib import Path
from typing import TYPE_CHECKING, Literal

from . import _runtime
from ._pipe import open_pipe
from ._session import PROTOCOL, ConnectionLostError, Session, Stream
from ._wire.protocol import Empty, StoreOptions, methods
from .errors import ClosedError, InvalidError, OutcomeUnknownError, UnavailableError

if TYPE_CHECKING:
    import os
    import ssl
    from collections.abc import Awaitable, Callable
    from datetime import datetime

Durability = Literal["full", "os"]
"""How far a commit goes before it returns.

``"full"`` is synced to the disk: it survives a crash of the operating system
and a power loss, and is what kv, jobs, files, search and every database are
unless told. ``"os"`` is written to the operating system and synced a little
later: it survives the program's crash, and a power loss may take the last
commits, never the file. Records and metrics are ``"os"`` unless told.
"""

HANDSHAKE_TIME = 5.0
START_TIME = 15.0
"""How long a client waits for a sidecar to answer, its own or another's, before it gives up."""
HELD_PAUSE = 0.1
"""The pause before a start that found the directory held starts again, doubling up to a second."""


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
        """the handles this connection opened, by their open's method and body"""

    async def handshake(self) -> None:
        try:
            await asyncio.wait_for(asyncio.shield(self.session.welcomed), HANDSHAKE_TIME)
        except BaseException:
            self._transport.close()
            self.session.end(ClosedError(f"no WELCOME within {HANDSHAKE_TIME:.0f} s"))
            raise
        self.session.on_end(lambda _: self._transport.close())

    async def close(self) -> None:
        """Ends the connection; a private child or the core in this process is waited for until it let go."""
        self.session.end(ClosedError("the store closed"))
        self._transport.close()
        if self._closing is not None:
            await self._closing()


@dataclass(frozen=True, slots=True)
class Started:
    """What a server or a core this process starts is started with; one found running keeps its own."""

    durability: Durability | None = None
    encryption_key_file: str | None = None
    idle: str | None = None
    """how long a sidecar stays once its last connection has gone, as tinystore serve --idle takes it"""
    self_metrics: bool = False
    """measures the store into its own metrics"""
    system_metrics: bool = True
    """with self_metrics, the machine it runs on too"""
    max_series: int | None = None
    """the metrics series the store holds at most, when not the store's own 100,000"""


def check_max_series(max_series: int | None) -> None:
    """Refuses a count of series no store can hold, before anything leaves."""
    if max_series is not None and (isinstance(max_series, bool) or max_series < 1):
        raise InvalidError(f"open: max_series {max_series!r}: a whole count above 0")


def check_durability(durability: str | None, of: str) -> None:
    """Refuses a durability that is neither word, before anything leaves."""
    if durability is not None and durability not in ("full", "os"):
        raise InvalidError(f"{of}: durability {durability!r}: it is 'full' or 'os'")


def asking(
    dial: Callable[[], Awaitable[Connection]], directory: str | os.PathLike[str], durability: Durability | None
) -> Callable[[], Awaitable[Connection]]:
    """Dials as dial does, and refuses a store whose files commit another way than this program asked for.

    A store has one durability, its first opener's, and a program that needs
    another must not be told it has it.
    """
    if durability is None:
        return dial

    async def checked() -> Connection:
        connection = await dial()
        opened = (await connection.session.welcomed).durability
        if opened != durability:
            await connection.close()
            said = (
                "with no durability set, full but os for records and metrics"
                if opened is None
                else f"with durability {opened!r}"
            )
            raise InvalidError(
                f"the store in {directory} is open {said}, and this program asked for {durability!r}: "
                f"open it without durability, or start its server with --durability {durability}"
            )
        return connection

    return checked


def _store_flags(started: Started) -> list[str]:
    """The flags of tinystore serve that say how the store opens."""
    flags: list[str] = []
    if started.durability is not None:
        flags += ["--durability", started.durability]
    if started.encryption_key_file is not None:
        flags += ["--encryption-key-file", str(Path(started.encryption_key_file).resolve())]
    if started.self_metrics:
        flags.append("--self-metrics")
        if not started.system_metrics:
            flags.append("--no-system-metrics")
    if started.max_series is not None:
        flags += ["--max-series", str(started.max_series)]
    return flags


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


def in_process(
    directory: str | os.PathLike[str], library: str | None, started: Started
) -> Callable[[], Awaitable[Connection]]:
    """The core in this process, through its pipe: no server and no socket."""
    absolute = str(Path(directory).resolve())
    key = started.encryption_key_file
    options = StoreOptions.encode(
        durability=started.durability,
        encryption_key_file=None if key is None else str(Path(key).resolve()),
        self_metrics=started.self_metrics,
        without_system=started.self_metrics and not started.system_metrics,
        max_series=started.max_series,
    )

    async def start() -> Connection:
        holder: list[Session] = []
        transport, closing = await open_pipe(
            library,
            absolute,
            options,
            lambda chunk: holder[0].receive(chunk) if holder else None,
            lambda err: holder[0].end(err) if holder else None,
        )
        session = Session(transport.write, _runtime.CLIENT)
        holder.append(session)
        connection = Connection(session, transport, closing)
        await connection.handshake()
        return connection

    return start


def sidecar(
    directory: str | os.PathLike[str],
    binary: Callable[[], str],
    started: Started,
    older: Callable[[str], bool] | None = None,
) -> Callable[[], Awaitable[Connection]]:
    """Finds the directory's sidecar through SERVE or starts one, as docs/wire.md says.

    A sidecar found proves itself before the first call; one started exits
    with 3 when another holds the directory, whose SERVE the client then
    waits for. A sidecar of an older release than this SDK's is stopped and
    replaced with this SDK's binary; another server of an older release, a
    person's or a program's own, is told of once.
    """
    absolute = Path(directory).resolve()
    serve = absolute / "server" / "SERVE"
    log = absolute / "server" / "serve.log"
    idling = [] if started.idle is None else ["--idle", started.idle]

    def is_older(server: str) -> bool:
        return older(server) if older is not None else older_release(server, _runtime.VERSION)

    told = False

    async def reach() -> Connection:
        nonlocal told
        found = await _reach_serve(serve)
        stopped_instance: str | None = None
        if found is not None:
            connection, is_sidecar, instance = found
            server = (await connection.session.welcomed).server
            if not is_older(server):
                return connection
            if is_sidecar and await _stopped(connection):
                print(replaced_sidecar(server, _runtime.VERSION, directory), file=sys.stderr)
                stopped_instance = instance
            else:
                if not told:
                    print(older_server(server, _runtime.VERSION, directory), file=sys.stderr)
                told = True
                return connection
        flags = [*idling, *_store_flags(started)]
        command = [binary(), "serve", "--dir", str(absolute), "--local", "--log-file", str(log), *flags]
        return await _start_sidecar(serve, log, command, stopped_instance)

    return reach


async def _start_sidecar(serve: Path, log: Path, command: list[str], stopped_instance: str | None = None) -> Connection:
    """Starts a sidecar and waits for SERVE to name one that answers, its own or another client's that won the start.

    One that exits with 3 found the directory held, by a winner about to
    publish or by a sidecar still letting go, and is started again after a
    pause; any other exit is an error. A sidecar told to stop answers for a
    moment after it agreed, until SERVE goes, so the one of the instance
    stopped is passed over.
    """
    loop = asyncio.get_running_loop()
    deadline = loop.time() + START_TIME
    held = False
    pause = HELD_PAUSE
    while True:
        child = _runtime.spawn_detached(command)
        again: float | None = None
        wait = 0.005
        while loop.time() < deadline:
            reached = await _reach_serve(serve)
            if reached is not None and reached[2] != stopped_instance:
                return reached[0]
            if reached is not None:
                await reached[0].close()
            code = child.poll()
            if code is not None and code != 3:
                raise ClosedError(f"the sidecar exited with {code}: {_tail(log)}")
            if code == 3:
                held = True
                if again is None:
                    again = loop.time() + pause
                if loop.time() >= again:
                    break
            await asyncio.sleep(wait)
            wait = min(wait * 2, 0.1)
        if loop.time() >= deadline:
            why = "another process holds the directory but no sidecar answers" if held else "no sidecar answered"
            raise ClosedError(f"{why} in {START_TIME:.0f} s: {_tail(log)}")
        pause = min(pause * 2, 1.0)


async def _stopped(connection: Connection) -> bool:
    """Asks a sidecar to stop as tinystore stop does, closing the connection once it has answered.

    False when it refuses, and the connection stays open.
    """
    try:
        await connection.session.call(methods["server.stop"], Empty.encode())
    except Exception:
        return False
    await connection.close()
    return True


def older_release(server: str, own: str) -> bool:
    """Whether a server's release is older than the SDK's own.

    A build that is no release, a development copy's or a Go pseudo-version,
    is neither.
    """
    theirs, ours = _release(server), _release(own)
    return theirs is not None and ours is not None and _compare_releases(theirs, ours) < 0


def replaced_sidecar(server: str, own: str, directory: str | os.PathLike[str]) -> str:
    """What a program whose SDK replaced a sidecar of an older release is told."""
    return (
        f"tinystore: the sidecar serving {directory} was {server}, older than this SDK's {own}; it finishes "
        "its calls, and this SDK's binary serves the directory from now on"
    )


def older_server(server: str, own: str, directory: str | os.PathLike[str]) -> str:
    """What a program whose SDK found a server of an older release that is no sidecar is told."""
    return (
        f"tinystore: {directory} is served by {server}, older than this SDK's {own}; a person or a program "
        "started that server, and it runs until they stop it"
    )


_RELEASE = re.compile(r"v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.]+))?")


def _release(version: str) -> tuple[list[int], list[str]] | None:
    """A release's version, its v optional, as npm or PyPI spell it; a development copy's 0.0.0 is none."""
    parts = _RELEASE.fullmatch(_npm_spelling(version))
    if parts is None:
        return None
    core = [int(parts[1]), int(parts[2]), int(parts[3])]
    if core == [0, 0, 0]:
        return None
    return core, [] if parts[4] is None else parts[4].split(".")


def _npm_spelling(version: str) -> str:
    """PyPI's 0.2.0rc1 as npm and Go spell it, 0.2.0-rc.1; any other version as it is."""
    pre = re.fullmatch(r"(\d+\.\d+\.\d+)(a|b|rc)(\d+)", version)
    if pre is None:
        return version
    return f"{pre[1]}-{ {'a': 'alpha', 'b': 'beta', 'rc': 'rc'}[pre[2]] }.{pre[3]}"


def _compare_releases(a: tuple[list[int], list[str]], b: tuple[list[int], list[str]]) -> int:
    """Semantic versioning's order: a release after its pre-releases, rc.2 before rc.10."""
    if a[0] != b[0]:
        return -1 if a[0] < b[0] else 1
    if not a[1] or not b[1]:
        return len(b[1]) - len(a[1])
    for x, y in zip(a[1], b[1], strict=False):
        if x == y:
            continue
        if x.isdigit() and y.isdigit():
            return int(x) - int(y)
        return -1 if x < y else 1
    return len(a[1]) - len(b[1])


async def _reach_serve(serve: Path) -> tuple[Connection, bool, str] | None:
    """The sidecar SERVE names, once its proof checks, whether SERVE calls it a sidecar, and its instance.

    None on any failure.
    """
    try:
        published = json.loads(await asyncio.to_thread(serve.read_bytes))
        endpoint = published["endpoints"][0]
        secret = base64.urlsafe_b64decode(published["secret"] + "==")
    except (OSError, ValueError, KeyError, IndexError, TypeError):
        return None
    if published.get("protocol") != PROTOCOL or len(secret) != 32:
        return None
    try:
        return await dial(endpoint, secret=secret), published.get("sidecar") is True, str(published.get("instance"))
    except Exception:
        return None


def _tail(log: Path) -> str:
    try:
        return "\n".join(log.read_text(encoding="utf-8", errors="replace").strip().splitlines()[-5:])
    except OSError:
        return "it wrote no log"


def private_child(
    directory: str | os.PathLike[str], binary: Callable[[], str], clock: datetime | None, started: Started
) -> Callable[[], Awaitable[Connection]]:
    """A private child: tinystore serve --stdio, living and dying with this process.

    A test's clock starts where the first child's did, and a child started
    again starts it there.
    """
    absolute = str(Path(directory).resolve())
    clocked = [] if clock is None else ["--clock", clock.astimezone(UTC).isoformat().replace("+00:00", "Z")]

    async def start() -> Connection:
        holder: list[Session] = []
        child = await _runtime.spawn_private(
            [binary(), "serve", "--dir", absolute, "--stdio", *clocked, *_store_flags(started)],
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


Idempotence = Literal["read", "write"]
"""What a call may do when its connection is lost: go again, or say its outcome is unknown."""


class Link:
    """Gives calls a connection, dialling again once the last one ended.

    A call whose REQUEST had not left goes again when its connection is lost,
    so does a read that had, and a write that had is OutcomeUnknownError.
    """

    def __init__(self, dialer: Callable[[], Awaitable[Connection]]) -> None:
        self._dialer = dialer
        self._current: asyncio.Future[Connection] | None = None
        self._closed = False

    @property
    def closed(self) -> bool:
        return self._closed

    async def connection(self) -> Connection:
        """The connection a call goes on."""
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
        # one dial at a time: every caller that found the same dead connection
        # waits for the one new one
        dialled = asyncio.ensure_future(self._dialer())
        self._current = dialled
        try:
            return await asyncio.shield(dialled)
        except BaseException:
            if self._current is dialled:
                self._current = None
            raise

    async def run[T](self, idempotence: Idempotence, attempt: Callable[[Connection], Awaitable[T]]) -> T:
        """Runs a call on the link's connection, going again as its idempotence lets it."""
        for tries in range(3):
            connection = await self.connection()
            try:
                return await attempt(connection)
            except (ConnectionLostError, UnavailableError) as err:
                if tries < 2 and goes_again(err, idempotence):
                    continue
                raise
        raise ClosedError("unreachable")

    async def close(self) -> None:
        """Closes the connection and every one to come."""
        self._closed = True
        current, self._current = self._current, None
        if current is not None:
            try:
                connection = await current
            except Exception:
                return
            await connection.close()


def goes_again(err: BaseException, idempotence: Idempotence) -> bool:
    """Whether a call that failed goes on another connection: one whose REQUEST had not left, and a read that had.

    A write that had is OutcomeUnknownError, raised here; any other failure is
    the call's own.
    """
    if not isinstance(err, ConnectionLostError | UnavailableError):
        return False
    sent = isinstance(err, ConnectionLostError) and err.sent
    if not sent or idempotence == "read":
        return True
    raise OutcomeUnknownError(
        f"the connection was lost with the write in flight; read what it wrote before writing again: {err}"
    ) from err


@dataclass
class Downloaded:
    """What a download sent: its header, its items, and the trailer that ended it."""

    header: bytes
    items: list[bytes]
    trailer: bytes


async def download(connection: Connection, method: int, body: bytes) -> Downloaded:
    """Runs a download to its end, giving each item's credit back as it arrives.

    A download the server answers with a RESPONSE that ends it has no items
    and that RESPONSE for trailer.
    """
    stream: Stream = await connection.session.open(method, body, True)
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


def piece_of(connection: Connection) -> int:
    """The bytes a piece carries: half what the server takes on a stream before it grants more, less its own few."""
    agreed = connection.session.agreed
    most = min(agreed.max_body, agreed.stream_credit) if agreed is not None else 1 << 20
    return max(1, most // 2 - 16)


_NAME = re.compile(r"[a-z0-9][a-z0-9_-]{0,63}")


def check_name(name: str, of: str) -> None:
    """Refuses a name the store would, before anything leaves, with the nearest name that is when there is one."""
    if not isinstance(name, str):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"the {of} name {name!r}: a name is [a-z0-9][a-z0-9_-]{{0,63}}")
    if not _NAME.fullmatch(name):
        nearest = re.sub(r"[^a-z0-9_-]", "_", name.lower()).lstrip("_-")[:64]
        offered = f", as {nearest!r}" if _NAME.fullmatch(nearest) else ""
        raise InvalidError(f"the {of} name {name!r}: a name is [a-z0-9][a-z0-9_-]{{0,63}}{offered}")
