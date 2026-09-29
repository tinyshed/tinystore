"""The byte streams and processes this SDK needs, through asyncio.

A Unix socket, a Windows named pipe through the Proactor loop, which CPython
serves there without AF_UNIX, TCP and TLS; a private child on stdin and
stdout; a sidecar that outlives this process.
"""

from __future__ import annotations

import asyncio
import os
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass
from importlib import resources
from typing import TYPE_CHECKING

from .errors import ClosedError

if TYPE_CHECKING:
    import ssl
    from collections.abc import Callable

VERSION = "0.1.0"
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
        remote = re.fullmatch(r"(tcp|tls)://(\[[^\]]+\]|[^:/]+):(\d+)", endpoint)
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
        return "".join(self.stderr)[-4000:].strip()


async def spawn_private(
    argv: list[str], data: Callable[[bytes], None], end: Callable[[BaseException], None]
) -> PrivateChild:
    """Starts a private server, reading its stdout into the session and its stderr to the end."""
    process = await asyncio.create_subprocess_exec(
        *argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE
    )
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

    async def read() -> None:
        draining = asyncio.ensure_future(drain())
        while chunk := await stdout.read(1 << 16):
            data(chunk)
        await draining
        code = await process.wait()
        end(ClosedError(f"the private server exited with {code}: {''.join(kept)[-2000:].strip()}"))

    def close() -> None:
        if not stdin.is_closing():
            stdin.close()

    return PrivateChild(Transport(stdin.write, close), process, kept, asyncio.ensure_future(read()))


def spawn_detached(argv: list[str]) -> subprocess.Popen[bytes]:
    """Starts a process that outlives this one, with no stdio to read, nor a console on Windows."""
    if sys.platform == "win32":  # pyright knows a platform only by this test
        flags = subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.CREATE_NO_WINDOW
        return subprocess.Popen(
            argv,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            creationflags=flags,
        )
    return subprocess.Popen(
        argv,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )


def find_binary(given: str | None) -> str:
    """The tinystore binary: the one given, TINYSTORE_BIN, this platform's wheel's, or PATH's."""
    found = given or os.environ.get("TINYSTORE_BIN")
    if not found:
        packaged = resources.files("tinystore") / "bin" / ("tinystore.exe" if WINDOWS else "tinystore")
        found = str(packaged) if packaged.is_file() else shutil.which("tinystore")
    if not found:
        raise ClosedError(
            "no tinystore binary: install this platform's wheel, put tinystore on PATH, or set TINYSTORE_BIN"
        )
    return found
