"""Directories, as docs/guides/blobs/directories.md has them.

A folder of the store's machine under a name, which the store lists and
reads and never writes. A scan writes down what appeared, changed or went;
changes keep a copy in step from a cursor; a read comes from the disk, held
to the version its get opened, or in a growing directory to none::

    movies = store.directory("movies", path="D:/Movies")
    scan = await movies.scan()
    page = await movies.changes(after=cursor)
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Literal

from ._connection import check_name
from ._handles import handle_on, via_link
from ._time import Duration, ms
from ._wire.protocol import (
    BlobsAt,
    BlobsChanges,
    BlobsDirectory,
    BlobsFolder,
    BlobsGot,
    BlobsHead,
    BlobsList,
    BlobsPage,
    BlobsScanned,
    BlobsSince,
    BlobsUsage,
    methods,
)
from .blobs import FileInfo, FilePage, ReadAt, StoredFile, Usage, info_of, read_stream, segment_of

if TYPE_CHECKING:
    from collections.abc import Callable

    from ._connection import Connection, Idempotence, Link

Change = Literal["added", "changed", "removed"]


@dataclass(frozen=True)
class Scan:
    """What a scan found, and what the list holds once it ended."""

    added: int
    changed: int
    removed: int
    skipped: int
    """what it met and could not list: a link, a folder it cannot read, another disk"""
    count: int
    size: int


@dataclass(frozen=True)
class FileChange:
    """A file's change since a cursor: its path in the directory, and what it carries now, None once removed."""

    path: str
    change: Change
    info: FileInfo | None


@dataclass(frozen=True)
class Changes:
    """A page of changes in the order the scans wrote them; next is always there."""

    items: list[FileChange]
    next: str


def open_directory(
    link: Link,
    name: str,
    path: str | None,
    every: Duration | None,
    ignore: list[str] | None,
    growing: bool,
) -> Directory:
    """A directory as ``store.directory`` opens it."""
    check_name(name, "directory")
    opener = BlobsDirectory.encode(
        name=name,
        path=path,
        every=None if every is None else ms(every, f"directory {name}: every"),
        ignore=list(ignore or []),
        growing=growing,
    )
    return Directory(link, name, opener, (), growing)


class Directory:
    """A directory, or a folder of it.

    get, head, list and usage are a set of files' calls with a set of files'
    answers, answered from the list the last scan wrote but a get's bytes,
    which come from the disk. Directories come from ``store.directory``.
    """

    def __init__(self, link: Link, name: str, opener: bytes, folder: tuple[str, ...], growing: bool) -> None:
        self.name = name
        self._link = link
        self._via = via_link(link)
        self._opener = opener
        self._folder = folder
        self._growing = growing

    def folder(self, *segments: str | int) -> Directory:
        """The folder of these segments, one each, whose paths are relative to it."""
        folder = (*self._folder, *(segment_of(s) for s in segments))
        return Directory(self._link, self.name, self._opener, folder, self._growing)

    async def scan(self) -> Scan:
        """Walks the folder and writes down what appeared, changed or went since the last scan."""
        answer = await self._call(
            "blobs.scan", lambda h: BlobsFolder.encode(handle=h, folder=list(self._folder)), "write"
        )
        scan = BlobsScanned.decode(answer)
        return Scan(
            added=scan.get("added", 0),
            changed=scan.get("changed", 0),
            removed=scan.get("removed", 0),
            skipped=scan.get("skipped", 0),
            count=scan.get("count", 0),
            size=scan.get("size", 0),
        )

    async def changes(self, *, after: str | None = None) -> Changes:
        """The changes past a cursor, a page's next, or from the first file the directory listed."""
        answer = await self._call("blobs.changes", lambda h: BlobsSince.encode(handle=h, after=after))
        page = BlobsChanges.decode(answer)
        items = [
            FileChange(
                path=item.get("path", ""),
                change=item.get("change", "added"),
                info=None if item.get("info") is None else info_of(item["info"]),
            )
            for item in page.get("items", [])
        ]
        return Changes(items, page.get("next", "0"))

    async def get(self, path: str) -> StoredFile | None:
        """The file as a scan listed it and as the disk holds it now; None when no scan listed it, or it is gone.

        A read of a file that moved since fails ConflictError, but in a
        growing directory, which reads up to the size now.
        """
        answer = await self._call("blobs.get", lambda h: BlobsAt.encode(handle=h, folder=list(self._folder), path=path))
        got = BlobsGot.decode(answer)
        found = got.get("info")
        if found is None:
            return None
        info = info_of(found)
        whole: bytes | None = got.get("bytes", b"")
        # a growing file's bytes come from the disk as it is when they are read
        if self._growing or whole is None or len(whole) != info.size:
            whole = None
        if_match = None if self._growing else info.etag
        return StoredFile(
            info,
            whole,
            lambda offset, length: read_stream(
                self._link, self._handle, ReadAt(self._folder, path, if_match, offset, length)
            ),
        )

    async def head(self, path: str) -> FileInfo | None:
        """What the last scan wrote down of the file; it reads no disk."""
        answer = await self._call(
            "blobs.head", lambda h: BlobsAt.encode(handle=h, folder=list(self._folder), path=path)
        )
        found = BlobsHead.decode(answer).get("info")
        return None if found is None else info_of(found)

    async def list(self, *, prefix: str = "", after: str | None = None, limit: int | None = None) -> FilePage:
        """A page of the folder's files, in the byte order of their paths, as the last scan wrote them."""
        answer = await self._call(
            "blobs.list",
            lambda h: BlobsList.encode(handle=h, folder=list(self._folder), prefix=prefix, after=after, limit=limit),
        )
        page = BlobsPage.decode(answer)
        return FilePage([info_of(each) for each in page.get("files", [])], page.get("next"))

    async def usage(self) -> Usage:
        """The files the last scan listed in the folder, and their bytes."""
        answer = await self._call("blobs.usage", lambda h: BlobsFolder.encode(handle=h, folder=list(self._folder)))
        usage = BlobsUsage.decode(answer)
        return Usage(usage.get("count", 0), usage.get("size", 0))

    async def forget(self) -> None:
        """Removes every file of the list, each told removed, and nothing on the disk."""
        await self._call("blobs.forget", lambda h: BlobsFolder.encode(handle=h, folder=list(self._folder)), "write")

    async def _handle(self, connection: Connection) -> int:
        return await handle_on(connection, methods["blobs.directory"], self._opener)

    async def _call(self, method: str, body: Callable[[int], bytes], idempotence: Idempotence = "read") -> bytes:
        return await self._via.call(methods["blobs.directory"], self._opener, methods[method], body, idempotence)
