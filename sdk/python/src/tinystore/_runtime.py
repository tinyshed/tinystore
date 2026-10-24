"""The byte streams and processes this SDK needs, through asyncio.

A Unix socket, a Windows named pipe through the Proactor loop, which CPython
serves there without AF_UNIX, TCP and TLS; a private child on stdin and
stdout; a sidecar that outlives this process, its stderr read while it starts.
"""

from __future__ import annotations

import asyncio
import os
import re
import shutil
import subprocess
import sys
import threading
from dataclasses import dataclass
from importlib import metadata, resources
from typing import TYPE_CHECKING

from .errors import ClosedError

if TYPE_CHECKING:
    import ssl
    from collections.abc import Callable


def _version() -> str:
    """The installed distribution's version, which a release stamps; 0.0.0 for a copy never installed."""
    try:
        return metadata.version("tinyshed-tinystore")
    except metadata.PackageNotFoundError:
        return "0.0.0"


VERSION = _version()
CLIENT = f"tinystore-py/{VERSION} python/{sys.version_info.major}.{sys.version_info.minor}"
WINDOWS = sys.platform == "win32"


class Transport:
    """A byte stream to a server: write never waits, close ends it."""

    def __init__(self, write: Callable[[bytes], None], close: Callable[[], None]) -> None:
        self.write = write
        self.close = close


class _Feed(asyncio.Protocol):
    def __init__(self, data: Callable[[bytes], None], end: Callable[[BaseException], None]) -> None:
        self._data, self._end = data, end

    def data_received(self, data: bytes) -> None:
        self._data(data)

    def eof_received(self) -> bool:
        self._end(ClosedError("the server ended the connection"))
        return False

    def connection_lost(self, exc: Exception | None) -> None:
        self._end(exc or ClosedError("the connection closed"))


_REMOTE = re.compile(r"(tcp|tls)://(\[[^\]]+\]|[^:/]+):(\d+)")


async def connect(
    endpoint: str,
    data: Callable[[bytes], None],
    end: Callable[[BaseException], None],
    tls: ssl.SSLContext | None = None,
) -> Transport:
    """Connects to unix://path, pipe:name, tcp://host:port or tls://host:port."""
    loop = asyncio.get_running_loop()

    def feed() -> _Feed:
        return _Feed(data, end)

    transport: asyncio.BaseTransport
    if endpoint.startswith("unix://"):
        transport, _ = await loop.create_unix_connection(feed, endpoint.removeprefix("unix://"))
    elif endpoint.startswith("pipe:"):
        pipe = rf"\\.\pipe\{endpoint.removeprefix('pipe:')}"
        transport, _ = await loop.create_pipe_connection(feed, pipe)  # type: ignore[attr-defined]
    else:
        remote = _REMOTE.fullmatch(endpoint)
        if remote is None:
            raise ClosedError(f"no transport for the endpoint {endpoint}")
        host, port = remote[2].strip("[]"), int(remote[3])
        if remote[1] == "tls":
            import ssl as tls_module

            context = tls or tls_module.create_default_context()
            transport, _ = await loop.create_connection(feed, host, port, ssl=context, server_hostname=host)
        else:
            transport, _ = await loop.create_connection(feed, host, port)
    stream: asyncio.WriteTransport = transport  # type: ignore[assignment]
    return Transport(stream.write, stream.close)


@dataclass
class PrivateChild:
    """A private server: a child whose stdin and stdout are the connection."""

    transport: Transport
    process: asyncio.subprocess.Process
    stderr: list[str]
    reading: asyncio.Future[None]

    def tail(self) -> str:
        return "".join(self.stderr)[-2000:].strip()


def not_started(err: OSError) -> str:
    """Why a process did not start, in the words every SDK uses: not found, permission denied, or the system's own."""
    if isinstance(err, FileNotFoundError):
        return "not found"
    if isinstance(err, PermissionError):
        return "permission denied"
    return err.strerror or str(err)


async def spawn_private(
    argv: list[str], data: Callable[[bytes], None], end: Callable[[BaseException], None]
) -> PrivateChild:
    """Starts a private server, reading its stdout into the session and its stderr to the end."""
    try:
        process = await asyncio.create_subprocess_exec(
            *argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE
        )
    except OSError as err:
        raise ClosedError(f"cannot start the private server {argv[0]}: {not_started(err)}") from err
    assert process.stdin is not None
    assert process.stdout is not None
    assert process.stderr is not None
    stdin, stdout, stderr = process.stdin, process.stdout, process.stderr
    kept: list[str] = []

    async def drain() -> None:
        # a pipe nobody drains stops the server once it fills
        while line := await stderr.readline():
            kept.append(line.decode(errors="replace"))
            del kept[:-50]

    def close() -> None:
        if not stdin.is_closing():
            stdin.close()

    async def read() -> None:
        draining = asyncio.ensure_future(drain())
        while chunk := await stdout.read(1 << 16):
            data(chunk)
        await draining
        code = await process.wait()
        end(ClosedError(f"the private server exited with {code}: {child.tail()}"))

    child = PrivateChild(Transport(stdin.write, close), process, kept, asyncio.ensure_future(read()))
    return child


STDERR_KEPT = 16 << 10
"""How much of a sidecar's stderr is kept while it starts, its last bytes, to say why it stopped."""


class Detached:
    """A sidecar this process started, which outlives it, its stderr read while it starts.

    A thread of its own reads stderr to its end, which comes once the sidecar
    exits. Nothing closes the pipe under it, since closing a pipe that another
    thread is blocked reading can hang: once the sidecar serves, the thread
    waits there until the sidecar or this process leaves.
    """

    def __init__(self, process: subprocess.Popen[bytes]) -> None:
        self.process = process
        self._kept = bytearray()
        self._keeping = threading.Lock()
        self._reading = threading.Thread(target=self._read, name="tinystore sidecar stderr", daemon=True)
        self._reading.start()

    def _read(self) -> None:
        stderr = self.process.stderr
        assert stderr is not None
        # os.read gives what came so far, where the buffer's read waits for all it asked
        while chunk := os.read(stderr.fileno(), 1 << 12):
            with self._keeping:
                self._kept += chunk
                del self._kept[:-STDERR_KEPT]

    def stderr(self) -> str:
        """The last of what it wrote to stderr so far."""
        with self._keeping:
            return self._kept.decode(errors="replace")

    async def ended(self) -> str:
        """What it wrote to stderr, once that has ended, as it does when the sidecar exits."""
        await asyncio.to_thread(self._reading.join, 1.0)
        return self.stderr()


def spawn_detached(argv: list[str]) -> Detached:
    """Starts a process that outlives this one, its stderr read while it starts, and no console on Windows."""
    try:
        if sys.platform == "win32":  # pyright knows a platform only by this test
            flags = subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.CREATE_NO_WINDOW
            process = subprocess.Popen(
                argv,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                creationflags=flags,
            )
        else:
            process = subprocess.Popen(
                argv,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
    except OSError as err:
        raise ClosedError(f"cannot start the sidecar {argv[0]}: {not_started(err)}") from err
    return Detached(process)


def packaged_binary() -> str | None:
    """The binary this platform's wheel carries; None in a wheel without one."""
    packaged = resources.files("tinystore") / "bin" / ("tinystore.exe" if WINDOWS else "tinystore")
    return str(packaged) if packaged.is_file() else None


def find_binary(given: str | None) -> str:
    """The tinystore binary: the one given, TINYSTORE_BIN, this platform's wheel's, or PATH's."""
    found = given or os.environ.get("TINYSTORE_BIN") or packaged_binary() or shutil.which("tinystore")
    if not found:
        raise ClosedError(
            "no tinystore binary: install this platform's wheel, put tinystore on PATH, or set TINYSTORE_BIN"
        )
    return found
