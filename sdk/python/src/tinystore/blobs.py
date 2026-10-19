"""The application's files, as docs/guides/blobs/README.md has them.

Files by path in named sets, written whole and replaced whole. A file's bytes
go in pieces within the credit of the side that takes them, and a whole read
fails CorruptError before its last piece when its bytes changed::

    avatars = store.files("avatars")
    info = await avatars.put(f"{user.id}.png", png, content_type="image/png")
    avatar = await avatars.get(f"{user.id}.png")
    data = await avatar.read() if avatar else None
"""

from __future__ import annotations

import asyncio
import io
import json
import re
from dataclasses import dataclass, field
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any, Protocol, Self, cast

from ._connection import check_name, piece_of
from ._handles import handle_on, via_link
from ._session import ConnectionLostError, Stream
from ._time import Duration, date_of, moment, ms, unix_ms
from ._wire.protocol import (
    BlobsAt,
    BlobsDelete,
    BlobsExpire,
    BlobsFolder,
    BlobsFound,
    BlobsGot,
    BlobsHead,
    BlobsInfo,
    BlobsList,
    BlobsMove,
    BlobsOpen,
    BlobsPage,
    BlobsPiece,
    BlobsRead,
    BlobsUsage,
    BlobsWrite,
    BlobsWritten,
    methods,
)
from .errors import ClosedError, InvalidError, OutcomeUnknownError

if TYPE_CHECKING:
    from collections.abc import AsyncIterable, AsyncIterator, Callable
    from types import TracebackType

    from ._connection import Connection, Idempotence, Link

Size = int | str
"""A size: bytes, or text in binary units, "64KiB" or "2GiB"; "2GB" is refused, being 10⁹ to some and 2³⁰ to others."""


class _Readable(Protocol):
    def read(self, size: int = -1, /) -> bytes: ...


type Body = bytes | bytearray | memoryview | str | _Readable | AsyncIterable[bytes]
"""What a write takes: bytes, text as UTF-8, a file object opened for bytes, or an async iterator of bytes."""


@dataclass(frozen=True, slots=True)
class FileInfo:
    """What a file carries: what serving it needs."""

    path: str
    """its path within the folder it was asked from"""
    size: int
    etag: str
    """the SHA-256 of its bytes in hex, quoted, as an HTTP ETag is"""
    content_type: str
    last_modified: datetime
    expires: datetime | None
    """None for a file that does not expire"""
    meta: dict[str, str] = field(default_factory=dict[str, str])


@dataclass(frozen=True, slots=True)
class FilePage:
    """A page of a folder's files in the byte order of their paths, and where the next starts."""

    files: list[FileInfo]
    next: str | None
    """None on the last page"""


@dataclass(frozen=True, slots=True)
class Usage:
    """The files of a folder and their bytes, as it is now."""

    count: int
    size: int


_BINARY = {"KiB": 1 << 10, "MiB": 1 << 20, "GiB": 1 << 30, "TiB": 1 << 40}
_SIZE = re.compile(r"(\d+(?:\.\d+)?)(KiB|MiB|GiB|TiB)")


def bytes_of(size: Size) -> int:
    """A size's bytes. "2GB" is refused: it is 10⁹ bytes to some and 2³⁰ to others."""
    if isinstance(size, bool) or not isinstance(size, int | str):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"a size of {size!r}")
    if isinstance(size, int):
        if size < 0:
            raise InvalidError(f"a size of {size} bytes")
        return size
    parts = _SIZE.fullmatch(size.strip())
    if parts is None:
        raise InvalidError(f"the size {size!r}; write it in bytes, or as 64KiB, 10MiB, 2GiB or 1TiB")
    return int(float(parts[1]) * _BINARY[parts[2]])


def _info_of(info: dict[str, Any]) -> FileInfo:
    return FileInfo(
        path=info.get("path", ""),
        size=info.get("size", 0),
        etag=info.get("etag", ""),
        content_type=info.get("content_type", ""),
        last_modified=moment(info.get("last_modified", 0)),
        expires=date_of(info.get("expires")),
        meta=dict(info.get("meta", {})),
    )


def _written_of(body: bytes) -> FileInfo | None:
    info = BlobsWritten.decode(body).get("info")
    return None if info is None else _info_of(info)


def _segment_of(segment: str | int) -> str:
    """A folder's segment as text, an integer its decimal spelling; the store refuses one that is not a segment."""
    if isinstance(segment, bool) or not isinstance(segment, int | str):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"the folder segment {segment!r}: a segment is text or an integer")
    return str(segment)


def open_files(link: Link, name: str, ttl: Duration | None, max_file_size: Size | None) -> Files:
    """Files as ``store.files`` opens them."""
    check_name(name, "files")
    opener = BlobsOpen.encode(
        name=name,
        ttl=None if ttl is None else ms(ttl, f"files {name}: ttl"),
        max_file_size=None if max_file_size is None else bytes_of(max_file_size),
    )
    return Files(link, name, opener, ())


class Files:
    """A named set of files by path, or a folder of it, whose calls are the folder's.

    A path in a folder is the folder's segments and then its own.
    """

    def __init__(self, link: Link, name: str, opener: bytes, folder: tuple[str, ...]) -> None:
        self.name = name
        self._link = link
        self._via = via_link(link)
        self._opener = opener
        self._folder = folder

    def folder(self, *segments: str | int) -> Files:
        """The folder of these segments, one each: ``folder("users", 42)`` is users/42, never users/420."""
        return Files(self._link, self.name, self._opener, (*self._folder, *(_segment_of(s) for s in segments)))

    async def put(
        self,
        path: str,
        body: Body,
        *,
        content_type: str | None = None,
        meta: dict[str, str] | None = None,
        ttl: Duration | None = None,
        if_match: str | None = None,
        size: int | None = None,
    ) -> FileInfo:
        """Writes the file whole and replaces whatever the path held.

        It returns once the file is on disk as the store's durability says.
        Nobody sees half of it, while it is written or after a crash.
        """
        written = await self._write(path, body, self._fields(path, content_type, meta, ttl, if_match, size), False)
        assert written is not None
        return written

    async def create(
        self,
        path: str,
        body: Body,
        *,
        content_type: str | None = None,
        meta: dict[str, str] | None = None,
        ttl: Duration | None = None,
        size: int | None = None,
    ) -> bool:
        """Writes the file only where the path holds none, and says whether it did."""
        fields = self._fields(path, content_type, meta, ttl, None, size)
        return await self._write(path, body, fields, True) is not None

    async def get(self, path: str) -> StoredFile | None:
        """What the file carries, and its bytes; None when the path holds none.

        A file up to about a megabyte comes with them, and a larger one's are
        read when they are read, on its ETag, as S3's GET with If-Match reads
        them.
        """
        answer = await self._call("blobs.get", lambda h: BlobsAt.encode(handle=h, folder=list(self._folder), path=path))
        got = BlobsGot.decode(answer)
        found = got.get("info")
        if found is None:
            return None
        info = _info_of(found)
        whole: bytes | None = got.get("bytes", b"")
        if whole is not None and len(whole) != info.size:
            whole = None
        return StoredFile(info, whole, lambda offset, length: self._read(path, info.etag, offset, length))

    async def head(self, path: str) -> FileInfo | None:
        """What the file carries and none of its bytes; None when the path holds none."""
        answer = await self._call(
            "blobs.head", lambda h: BlobsAt.encode(handle=h, folder=list(self._folder), path=path)
        )
        found = BlobsHead.decode(answer).get("info")
        return None if found is None else _info_of(found)

    async def delete(self, path: str, *, if_match: str | None = None) -> None:
        """Deletes the file, or with if_match only that version of it; no file there is not an error."""
        await self._call(
            "blobs.delete",
            lambda h: BlobsDelete.encode(handle=h, folder=list(self._folder), path=path, if_match=if_match),
            "write",
        )

    async def copy(self, source: str, to: str, *, if_match: str | None = None) -> FileInfo:
        """A copy at to that shares the bytes, which are not copied.

        A file at to is ConflictError unless if_match names the version to
        replace.
        """
        return await self._move("blobs.copy", source, to, if_match)

    async def rename(self, source: str, to: str, *, if_match: str | None = None) -> FileInfo:
        """The file moved to to in one write.

        A file there is ConflictError unless if_match names the version to
        replace, and none at source NotFoundError.
        """
        return await self._move("blobs.rename", source, to, if_match)

    async def expire(self, path: str, after: Duration | datetime) -> bool:
        """Sets when the file expires, a span from now or a time; says whether there was a file."""
        if isinstance(after, datetime):
            span = max(0, unix_ms(after, "a file's expiry") - unix_ms(datetime.now(UTC)))
        else:
            span = ms(after, "a file's expiry")
        answer = await self._call(
            "blobs.expire",
            lambda h: BlobsExpire.encode(handle=h, folder=list(self._folder), path=path, after=span),
            "write",
        )
        return BlobsFound.decode(answer).get("found", False)

    async def list(self, *, prefix: str = "", after: str | None = None, limit: int | None = None) -> FilePage:
        """A page of the files in the folder and in the folders in it, in the byte order of their paths."""
        answer = await self._call(
            "blobs.list",
            lambda h: BlobsList.encode(handle=h, folder=list(self._folder), prefix=prefix, after=after, limit=limit),
        )
        page = BlobsPage.decode(answer)
        return FilePage([_info_of(each) for each in page.get("files", [])], page.get("next"))

    async def all(self, *, prefix: str = "") -> AsyncIterator[FileInfo]:
        """Every file of the folder, a page at a time, holding nothing between pages."""
        after: str | None = None
        while True:
            page = await self.list(prefix=prefix, after=after)
            for file in page.files:
                yield file
            if page.next is None:
                return
            after = page.next

    async def usage(self) -> Usage:
        """The files in the folder and their bytes, as it is now: it counts, it does not limit."""
        answer = await self._call("blobs.usage", lambda h: BlobsFolder.encode(handle=h, folder=list(self._folder)))
        usage = BlobsUsage.decode(answer)
        return Usage(usage.get("count", 0), usage.get("size", 0))

    async def clear(self) -> None:
        """Removes the folder's every file and every folder in it, at once for every reader."""
        await self._call("blobs.clear", lambda h: BlobsFolder.encode(handle=h, folder=list(self._folder)), "write")

    def upload(
        self,
        path: str,
        *,
        content_type: str | None = None,
        meta: dict[str, str] | None = None,
        ttl: Duration | None = None,
        if_match: str | None = None,
        size: int | None = None,
    ) -> Upload:
        """A file written a piece at a time, which appears at commit and never before.

        ``async with files.upload(path) as upload``, and leaving the block
        without commit aborts it.
        """
        fields = self._fields(path, content_type, meta, ttl, if_match, size)

        async def begin() -> tuple[Stream, int]:
            connection = await self._link.connection()
            write = BlobsWrite.encode(fields, handle=await self._handle(connection))
            stream = await connection.session.open(methods["blobs.upload"], write, False)
            return stream, piece_of(connection)

        return Upload(begin)

    async def _handle(self, connection: Connection) -> int:
        return await handle_on(connection, methods["blobs.open"], self._opener)

    async def _call(self, method: str, body: Callable[[int], bytes], idempotence: Idempotence = "read") -> bytes:
        return await self._via.call(methods["blobs.open"], self._opener, methods[method], body, idempotence)

    async def _read(self, path: str, if_match: str, offset: int | None, length: int | None) -> AsyncIterator[bytes]:
        """A file's bytes as S3's GET reads them with a range and If-Match.

        On the ETag a get gave, ConflictError once the path holds another file
        or none; the whole file checked, a range not. Nothing is asked before
        the first read.
        """

        async def open_read(connection: Connection) -> tuple[Stream, bytes, bool]:
            read = BlobsRead.encode(
                handle=await self._handle(connection),
                folder=list(self._folder),
                path=path,
                if_match=if_match,
                offset=offset,
                length=length,
            )
            stream = await connection.session.open(methods["blobs.read"], read, True)
            first = await stream.next()
            return stream, first.body, first.end

        stream, first, end = await self._link.run("read", open_read)
        try:
            # the RESPONSE's piece is no DATA, so it takes none of the credit
            piece = BlobsPiece.decode(first).get("bytes", b"")
            if piece:
                yield piece
            while not end:
                event = await stream.next()
                end = event.end
                if end:
                    return
                stream.consumed(len(event.body))
                piece = BlobsPiece.decode(event.body).get("bytes", b"")
                if piece:
                    yield piece
        finally:
            stream.cancel()

    async def _move(self, method: str, source: str, to: str, if_match: str | None) -> FileInfo:
        answer = await self._call(
            method,
            lambda h: BlobsMove.encode({"from": source}, handle=h, folder=list(self._folder), to=to, if_match=if_match),
            "write",
        )
        return _info_of(BlobsInfo.decode(answer))

    def _fields(
        self,
        path: str,
        content_type: str | None,
        meta: dict[str, str] | None,
        ttl: Duration | None,
        if_match: str | None,
        size: int | None,
    ) -> dict[str, Any]:
        return {
            "folder": list(self._folder),
            "path": path,
            "content_type": content_type,
            "meta": meta,
            "ttl": None if ttl is None else ms(ttl, f"files {self.name}: a file's ttl"),
            "if_match": if_match,
            "size": size,
        }

    async def _write(self, path: str, body: Body, fields: dict[str, Any], create: bool) -> FileInfo | None:
        """Writes a body: in one call when it fits a piece, else as an upload over a stream.

        It gives what it wrote, None when a create found a file.
        """
        source = _source_of(body)
        fields = {**fields, "create": create or None}
        if fields["size"] is None:
            fields["size"] = source.size

        async def attempt(connection: Connection) -> FileInfo | None:
            handle = await self._handle(connection)
            piece = piece_of(connection)
            whole = await source.whole(piece)
            if whole is not None:
                write = BlobsWrite.encode(fields, handle=handle, bytes=whole)
                return _written_of(await connection.session.call(methods["blobs.put"], write))
            write = BlobsWrite.encode(fields, handle=handle)
            stream = await connection.session.open(methods["blobs.upload"], write, False)
            return await _uploaded(stream, source, piece)

        return await self._link.run("write", attempt)


class _Source:
    """A body as its bytes come, its size when it says it, and whether it reads again from its start."""

    def __init__(
        self,
        size: int | None,
        replayable: bool,
        chunks: Callable[[], AsyncIterator[bytes]],
        whole: Callable[[int], Any],
    ) -> None:
        self.size = size
        self.replayable = replayable
        self.chunks = chunks
        self.whole = whole


async def _one(data: bytes) -> AsyncIterator[bytes]:
    if data:
        yield data


def _source_of(body: Body) -> _Source:
    if isinstance(body, str):
        body = body.encode()
    if isinstance(body, bytes | bytearray | memoryview):
        data = bytes(cast("bytes", body))

        async def fits(piece: int) -> bytes | None:
            return data if len(data) <= piece else None

        return _Source(len(data), True, lambda: _one(data), fits)
    if hasattr(body, "read"):
        return _file_source(cast("_Readable", body))
    stream = cast("AsyncIterable[bytes]", body)

    async def never(piece: int) -> bytes | None:
        return None

    return _Source(None, False, stream.__aiter__, never)


def _file_source(file: _Readable) -> _Source:
    """A file object's bytes, read on a thread, from its start again for a second try when it seeks."""
    seekable = isinstance(file, io.IOBase) and file.seekable()
    start = file.tell() if seekable and isinstance(file, io.IOBase) else 0

    async def chunks() -> AsyncIterator[bytes]:
        if seekable and isinstance(file, io.IOBase):
            await asyncio.to_thread(file.seek, start)
        while chunk := await asyncio.to_thread(file.read, 1 << 20):
            yield chunk

    async def never(piece: int) -> bytes | None:
        return None

    return _Source(None, seekable, chunks, never)


async def _pieces(chunks: AsyncIterator[bytes], size: int) -> AsyncIterator[bytes]:
    """The bytes of chunks again in pieces of size bytes, the last shorter."""
    held = bytearray()
    async for chunk in chunks:
        held += chunk
        while len(held) >= size:
            yield bytes(held[:size])
            del held[:size]
    if held:
        yield bytes(held)


async def _uploaded(stream: Stream, source: _Source, piece: int) -> FileInfo | None:
    """Sends an upload's pieces as DATA within the stream's credit, the last ending them.

    It waits for what the commit wrote. A connection lost before that last
    DATA left aborted the upload, which left nothing, and a body that reads
    again goes again on the next connection; lost after it, the commit's
    outcome is unknown.
    """
    ending = False
    try:
        last: bytes | None = None
        async for each in _pieces(source.chunks(), piece):
            if last is not None:
                await stream.send(BlobsPiece.encode(bytes=last), False)
            last = each
        ending = True
        await stream.send(BlobsPiece.encode(bytes=last or b""), True)
        # the RESPONSE that began it, and then what the commit wrote
        await stream.next()
        return _written_of((await stream.next()).body)
    except ConnectionLostError as err:
        if not ending and source.replayable:
            # the store aborted what it had of it, so the whole body goes again
            raise ConnectionLostError(str(err), sent=False) from err
        stream.cancel()
        raise _lost_as(err, ending) from err
    except BaseException:
        stream.cancel()
        raise


def _lost_as(err: ConnectionLostError, ending: bool) -> BaseException:
    """What a lost connection means for an upload: nothing left before its last DATA, unknown after."""
    if ending:
        return OutcomeUnknownError(f"the connection was lost as the upload committed: {err}")
    return ClosedError(f"the connection was lost during the upload, which left nothing: {err}")


class Upload:
    """A file written a piece at a time, as ``files.upload`` begins it.

    Bytes go in with write, the file appears at commit and never before, and
    an upload that ends otherwise leaves nothing.
    """

    def __init__(self, begin: Callable[[], Any]) -> None:
        self._begin = begin
        self._stream: Stream | None = None
        self._piece = 1 << 20
        self._held = bytearray()
        self._done = False

    async def __aenter__(self) -> Self:
        self._stream, self._piece = await self._begin()
        return self

    async def __aexit__(
        self, kind: type[BaseException] | None, error: BaseException | None, trace: TracebackType | None
    ) -> None:
        await self.abort()

    async def write(self, data: bytes | str) -> None:
        """Takes bytes, text as UTF-8, waiting while the store catches up."""
        stream = self._open()
        self._held += data.encode() if isinstance(data, str) else data
        try:
            while len(self._held) >= self._piece:
                chunk = bytes(self._held[: self._piece])
                del self._held[: self._piece]
                await stream.send(BlobsPiece.encode(bytes=chunk), False)
        except ConnectionLostError as err:
            raise _lost_as(err, False) from err

    async def commit(self) -> FileInfo:
        """Publishes the file whole, replacing what the path held as put does, once it is on disk."""
        stream = self._open()
        self._done = True
        try:
            await stream.send(BlobsPiece.encode(bytes=bytes(self._held)), True)
            # the RESPONSE that began it, and then the file the commit wrote
            await stream.next()
            written = _written_of((await stream.next()).body)
        except ConnectionLostError as err:
            raise _lost_as(err, True) from err
        assert written is not None
        return written

    async def abort(self) -> None:
        """Ends the upload, which leaves nothing; after commit it does nothing."""
        if self._done or self._stream is None:
            return
        self._done = True
        self._stream.cancel()

    def _open(self) -> Stream:
        if self._done:
            raise InvalidError("the upload has ended")
        if self._stream is None:
            raise InvalidError("an upload is written inside its async with")
        return self._stream


class FileBytes:
    """Bytes of a file, read when they are read: ``async for chunk in file``, or all of them with read()."""

    def __init__(self, size: int, chunks: Callable[[], AsyncIterator[bytes]]) -> None:
        self.size = size
        self._chunks = chunks

    def __aiter__(self) -> AsyncIterator[bytes]:
        """Its bytes as they come, the credit of each given back once it is taken; each a read of its own."""
        return self._chunks()

    async def read(self) -> bytes:
        """Every byte, held in memory: a large file is read a chunk at a time with async for."""
        whole = bytearray()
        async for chunk in self._chunks():
            whole += chunk
        return bytes(whole)

    async def text(self) -> str:
        return (await self.read()).decode()

    async def json(self) -> Any:
        return json.loads(await self.read())


class StoredFile(FileBytes):
    """A file as get found it: what it carries, and its bytes.

    A whole read is checked against its SHA-256 and fails CorruptError
    before its last bytes when they changed; slice reads a range, which is
    not checked.
    """

    def __init__(
        self, info: FileInfo, whole: bytes | None, read: Callable[[int | None, int | None], AsyncIterator[bytes]]
    ) -> None:
        super().__init__(info.size, (lambda: _one(whole)) if whole is not None else (lambda: read(None, None)))
        self.info = info
        self.path = info.path
        self.etag = info.etag
        self.content_type = info.content_type
        self.last_modified = info.last_modified
        self.expires = info.expires
        self.meta = info.meta
        self._whole = whole
        self._range = read

    def slice(self, start: int | None = None, end: int | None = None) -> FileBytes:
        """A range of its bytes, as Python slices them: slice(-500) is the last 500 bytes."""
        first, last, _ = slice(start, end).indices(self.size)
        last = max(first, last)
        whole = self._whole
        if whole is not None:
            part = whole[first:last]
            return FileBytes(len(part), lambda: _one(part))
        return FileBytes(last - first, lambda: self._range(first, last - first))
