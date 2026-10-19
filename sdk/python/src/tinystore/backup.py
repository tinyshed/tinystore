"""A backup of the whole store, as docs/guides/backup/README.md has it.

One zip of every engine that the server writes while the store keeps working
and sends in pieces, written beside the name the program gives and renamed
once whole, so that a backup that fails leaves no zip::

    backup = await store.backup("backup-2026-10-13.zip")
"""

from __future__ import annotations

import asyncio
import contextlib
import os
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any, BinaryIO

from ._wire.protocol import BackupPiece, BackupWrite, BackupWritten, methods

if TYPE_CHECKING:
    from collections.abc import Sequence

    from ._connection import Connection, Link


@dataclass(frozen=True, slots=True)
class Backup:
    """What a backup holds."""

    files: int
    """the files, the engines' and those it was told to take"""
    bytes: int
    """their bytes, the zip's own fields aside"""
    key_left_out: bool
    """the store seals values with an encryption key the backup does not hold: they open with that key alone"""


async def backup(
    link: Link, path: str | os.PathLike[str], include: Sequence[str], include_encryption_key: bool
) -> Backup:
    """Writes a backup of the store link reaches to path, beside it first.

    A connection lost on the way makes it again from its start.
    """
    final = Path(path)
    partial = final.with_name(f"{final.name}.{os.getpid()}.part")
    asked = BackupWrite.encode(include=list(include), include_encryption_key=include_encryption_key)

    async def attempt(connection: Connection) -> dict[str, Any]:
        file: BinaryIO = await asyncio.to_thread(partial.open, "wb")
        try:
            stream = await connection.session.open(methods["backup.write"], asked, True)
            await stream.next()
            while True:
                event = await stream.next()
                if event.end:
                    await asyncio.to_thread(_synced, file)
                    return BackupWritten.decode(event.body)
                stream.consumed(len(event.body))
                await asyncio.to_thread(file.write, BackupPiece.decode(event.body).get("bytes", b""))
        finally:
            await asyncio.to_thread(file.close)

    try:
        written = await link.run("read", attempt)
        await asyncio.to_thread(partial.replace, final)
    except BaseException:
        with contextlib.suppress(OSError):
            partial.unlink(missing_ok=True)
        raise
    return Backup(written.get("files", 0), written.get("bytes", 0), written.get("key_left_out", False))


def _synced(file: BinaryIO) -> None:
    file.flush()
    os.fsync(file.fileno())
