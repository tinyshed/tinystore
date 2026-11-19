"""A store for a test: a directory of its own, the core in this process, removed at close.

from tinystore.testing import open_temp

async with open_temp(clock=datetime(2026, 11, 19, 9, tzinfo=UTC)) as store:
    await store.clock.advance("1h")
"""

from __future__ import annotations

import asyncio
import shutil
import tempfile
import time
from pathlib import Path
from typing import TYPE_CHECKING, Any

from .errors import InUseError
from .store import Store, opened_as

if TYPE_CHECKING:
    from collections.abc import Generator
    from types import TracebackType

    from ._connection import Link

_LET_GO = 1.0
"""How long a removal tries again while a file stays held: Windows lets go of a closed file a moment late."""


class TempStore(Store):
    """A store in a directory of its own, which its close removes."""

    def __init__(self, link: Link, directory: Path) -> None:
        super().__init__(link)
        self.dir = directory
        """The store's directory, for a test that looks at its files."""

    async def close(self) -> None:
        """Closes the store, then removes its directory.

        A file still held a second after the close is a handle the test
        leaked, and fails the close with InUseError rather than leave the
        directory behind unsaid.
        """
        await super().close()
        await _removed(self.dir)


class _Opening:
    """An open_temp, awaited for the store or entered with ``async with``, which closes it at the end."""

    def __init__(self, options: dict[str, Any]) -> None:
        self._options = options
        self._store: TempStore | None = None

    def __await__(self) -> Generator[Any, None, TempStore]:
        return self._open().__await__()

    async def __aenter__(self) -> TempStore:
        self._store = await self._open()
        return self._store

    async def __aexit__(
        self, kind: type[BaseException] | None, error: BaseException | None, trace: TracebackType | None
    ) -> None:
        if self._store is not None:
            await self._store.close()

    async def _open(self) -> TempStore:
        directory = Path(tempfile.mkdtemp(prefix="tinystore-"))
        options: dict[str, Any] = {"embedded": not self._options.get("private", False), **self._options}
        try:
            return await opened_as(lambda link: TempStore(link, directory), directory, **options)
        except BaseException:
            await _removed(directory)
            raise


def open_temp(**options: Any) -> _Opening:
    """Opens a store in a new, empty directory under the system's temporary one, which its close removes.

    The directory is named ``tinystore-`` and a random part (contract
    STORE-12). It opens embedded, the core in this process, unless
    ``private=True`` asks for a child; every keyword of ``open`` passes
    through, ``clock`` among them. Await it for the store, or enter it with
    ``async with``, which closes the store at the end.
    """
    return _Opening(options)


async def _removed(directory: Path) -> None:
    deadline = time.monotonic() + _LET_GO
    while True:
        try:
            shutil.rmtree(directory)
            return
        except FileNotFoundError:
            return
        except OSError as err:
            if time.monotonic() > deadline:
                raise InUseError(f"open_temp {directory}: a file stays held after the store closed: {err}") from err
            await asyncio.sleep(0.02)


__all__ = ["TempStore", "open_temp"]
