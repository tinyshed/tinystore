"""Each way a store is reached, for the suites that run through all of them.

The core in this process, once ``just python-core`` has put the extension
module beside the sources, and a private child, a sidecar and a remote
server, when TINYSTORE_BIN names the tinystore that ``just build`` made.
"""

from __future__ import annotations

import asyncio
import importlib.util
import json
import os
import shutil
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

import tinystore

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

BINARY = os.environ.get("TINYSTORE_BIN")
SERVED = BINARY is not None and BINARY != "unused" and Path(BINARY).is_file()
CORE = importlib.util.find_spec("tinystore._core") is not None

TOKEN = "4dGQ6tR2kq0SxVZpLWn8E1yHf7cJmB3aUoN9Tz5Ki0E"
"""A remote server's admin token, in a tokens file of the test's own."""


@dataclass(frozen=True)
class Way:
    """A way a store is reached: how a test opens one in a directory, and lets go of it after."""

    name: str
    ready: bool
    open: Callable[[Path], Awaitable[tinystore.Store]]
    leave: Callable[[Path], Awaitable[None]]
    """waits until whatever served the directory has let go of it"""
    shared: bool = False
    """whether a second open of the directory joins the same store"""
    home: Callable[[Path], Path] = lambda directory: directory
    """the store's own directory, where a test's directory holds more than the store"""


async def _nothing(_: Path) -> None:
    return None


async def _sidecar_gone(directory: Path) -> None:
    await until(lambda: not (directory / "server" / "SERVE").exists())


_remotes: dict[Path, subprocess.Popen[bytes]] = {}


async def _remote(directory: Path) -> tinystore.Store:
    assert BINARY is not None
    (directory / "tokens").write_text(f"admin {TOKEN}\n")
    serve = directory / "store" / "server" / "SERVE"
    _remotes[directory] = subprocess.Popen(
        [
            BINARY,
            "serve",
            str(directory / "store"),
            "--listen",
            "tcp://127.0.0.1:0",
            "--tokens",
            str(directory / "tokens"),
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    await until(serve.exists)
    endpoints: list[str] = json.loads(serve.read_text())["endpoints"]
    return await tinystore.connect(endpoints[1], token=TOKEN)


async def _remote_gone(directory: Path) -> None:
    server = _remotes.pop(directory, None)
    if server is not None:
        server.kill()
        await asyncio.to_thread(server.wait)


WAYS = [
    Way("the core in this process", CORE, lambda d: tinystore.open(d, embedded=True), _nothing, shared=True),
    Way("a private child", SERVED, lambda d: tinystore.open(d, private=True), _nothing),
    # gone at once once the store closes, so that its directory can go
    Way("a sidecar", SERVED, lambda d: tinystore.open(d, idle="1ms"), _sidecar_gone, shared=True),
    Way("a remote server over TCP", SERVED, _remote, _remote_gone, home=lambda d: d / "store"),
]


async def until(done: Callable[[], bool], within: float = 10) -> None:
    deadline = time.monotonic() + within
    while not done() and time.monotonic() < deadline:
        await asyncio.sleep(0.02)


async def removed(directory: Path) -> None:
    """Removes a directory, again while Windows still holds a file in it a moment after its server."""
    deadline = time.monotonic() + 10
    while True:
        try:
            shutil.rmtree(directory)
            return
        except FileNotFoundError:
            return
        except OSError:
            if time.monotonic() > deadline:
                raise
            await asyncio.sleep(0.02)
