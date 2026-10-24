"""The core in this process: its pipe, through the extension module ``tinystore._core``.

The module is the core's library, crates/ffi built with its python feature,
which this platform's wheel carries; ``just python-core`` puts one built from
the checkout in its place.

The frames go in from the event loop's thread and come out on one thread of
this module's, which waits in ``recv`` with the interpreter's lock let go and
hands each read to the loop. No thread of the core's ever waits for Python:
the core only ever wakes the one waiting in ``recv``.
"""

from __future__ import annotations

import asyncio
import importlib
import importlib.machinery
import importlib.util
import sys
import threading
from typing import TYPE_CHECKING, Protocol

from ._runtime import Transport
from ._wire import frame as f
from ._wire.protocol import GoAway
from .errors import ClosedError, error_of

if TYPE_CHECKING:
    from collections.abc import Callable
    from types import ModuleType

_READ_WAIT_MS = 1000
"""How long the reading thread waits in one call to ``recv``; the end of the connection wakes it at once."""


class _Pipe(Protocol):
    def send(self, frames: bytes) -> None: ...
    def recv(self, wait_ms: int) -> bytes | None: ...
    def close(self) -> None: ...


def load_core(library: str | None) -> ModuleType:
    """The extension module: the library given, or this wheel's own."""
    if library is None:
        try:
            return importlib.import_module("tinystore._core")
        except ImportError as err:
            raise ClosedError(
                "no core in this install of tinystore: install this platform's wheel, or pass library"
            ) from err
    found = library
    loaded = sys.modules.get("tinystore._core")
    if loaded is not None:
        return loaded
    loader = importlib.machinery.ExtensionFileLoader("tinystore._core", found)
    spec = importlib.util.spec_from_file_location("tinystore._core", found, loader=loader)
    if spec is None:
        raise ClosedError(f"cannot load the core from {found}")
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    sys.modules["tinystore._core"] = module
    return module


async def open_pipe(
    library: str | None,
    directory: str,
    options: bytes,
    data: Callable[[bytes], None],
    end: Callable[[BaseException], None],
) -> tuple[Transport, Callable[[], asyncio.Future[None]]]:
    """Opens the store in this process and starts reading it.

    It gives the transport and what closes it once the core has let the
    directory go.
    """
    core = load_core(library)
    opening = asyncio.ensure_future(asyncio.to_thread(core.open, directory, options))
    try:
        # a wait, not a shield: 3.14 logs the error of a shielded open whose dial was cancelled
        await asyncio.wait((opening,))
    except asyncio.CancelledError:
        # the thread opens the store all the same: what it opens is let go
        opening.add_done_callback(_let_go)
        raise
    try:
        pipe: _Pipe = opening.result()
    except ConnectionError as err:
        code, message = err.args if len(err.args) == 2 else ("closed", str(err))
        raise error_of(code, f"cannot open {directory}: {message}") from None
    loop = asyncio.get_running_loop()
    reading = threading.Thread(target=_read, args=(pipe, loop, data, end), name="tinystore pipe", daemon=True)
    reading.start()
    closed = False

    def close() -> None:
        nonlocal closed
        if not closed:
            closed = True
            # the client's GOAWAY ends the session at once, which wakes the thread in recv
            pipe.send(f.frame(f.GOAWAY, 0, 0, 0, GoAway.encode(code="closed", message="the store closed")))

    def closing() -> asyncio.Future[None]:
        close()

        def let_go() -> None:
            reading.join()
            pipe.close()

        return asyncio.ensure_future(asyncio.to_thread(let_go))

    return Transport(pipe.send, close), closing


def _let_go(opened: asyncio.Future[_Pipe]) -> None:
    """Closes a pipe the core opened for a dial cancelled meanwhile."""
    if not opened.cancelled() and opened.exception() is None:
        opened.result().close()


def _read(
    pipe: _Pipe,
    loop: asyncio.AbstractEventLoop,
    data: Callable[[bytes], None],
    end: Callable[[BaseException], None],
) -> None:
    try:
        while (chunk := pipe.recv(_READ_WAIT_MS)) is not None:
            if chunk:
                loop.call_soon_threadsafe(data, chunk)
        loop.call_soon_threadsafe(end, ClosedError("the store closed"))
    except RuntimeError:
        # the loop closed before the store did: nobody is left to read
        return
