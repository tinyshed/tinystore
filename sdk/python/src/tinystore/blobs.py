"""The application's files in blobs/, as Go's blobs keeps them.

Objects under keys that are paths, their bytes streamed both ways in DATA of
64 KiB at most, a whole read checked against the object's hash.
"""

from __future__ import annotations

import asyncio
import os
import typing
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any, BinaryIO

from ._connection import Connection, Link, check_name, download, handle_on
from ._session import LostError, Stream
from ._time import Duration, date_of, ms, unix_ms
from ._wire.messages import METHODS, BlobsBucket, BlobsCall, BlobsObject, BlobsPage, BlobsTotal
from .errors import ClosedError, OutcomeUnknownError

if TYPE_CHECKING:
    from collections.abc import AsyncIterable, AsyncIterator, Iterable
    from datetime import datetime

    from ._wire.codec import Key

_CHUNK = 64 << 10

type Body = bytes | str | BinaryIO | os.PathLike[str] | Iterable[bytes] | AsyncIterable[bytes]
"""What a put takes: bytes, text, a file, a path, or an iterable of bytes, async or not."""


@dataclass(frozen=True, slots=True)
class BlobObject:
    key: str
    size: int
    etag: str
    """quoted, as an HTTP header carries it: equal bytes, one ETag"""
    content_type: str | None
    modified: datetime | None
    expires: datetime | None
    meta: dict[str, str]


@dataclass(frozen=True, slots=True)
class Usage:
    objects: int
    bytes: int


def _object(o: dict[str, Any]) -> BlobObject:
    meta = {k: v if isinstance(v, str) else v.decode(errors="replace") for k, v in o.get("meta", {}).items()}
    return BlobObject(
        o.get("key", ""),
        o.get("size", 0),
        o.get("etag", ""),
        o.get("content_type"),
        date_of(o.get("modified")),
        date_of(o.get("expires")),
        meta,
    )


def _write_fields(
    content_type: str | None,
    meta: dict[str, str] | None,
    ttl: Duration | None,
    expire_at: datetime | None,
    if_match: str | None,
    if_none_match: bool,
) -> dict[str, Any]:
    return {
        "content_type": content_type,
        "meta": meta,
        "ttl": None if ttl is None else ms(ttl),
        "expire_at": None if expire_at is None else unix_ms(expire_at),
        "if_match": if_match,
        "if_none_match": True if if_none_match else None,
    }


class Download:
    """An object being read: what it is at once, its bytes as they are iterated or read."""

    def __init__(self, obj: BlobObject, stream: Stream) -> None:
        self.object = obj
        self._stream = stream
        self._done = False

    def __aiter__(self) -> AsyncIterator[bytes]:
        return self._chunks()

    async def _chunks(self) -> AsyncIterator[bytes]:
        try:
            while not self._done:
                event = await self._stream.next()
                self._stream.consumed(len(event.body))
                self._done = event.end
                if event.body:
                    yield event.body
        finally:
            await self.aclose()

    async def read(self) -> bytes:
        """The whole object; a whole read whose bytes changed ends with CorruptError instead."""
        return b"".join([chunk async for chunk in self])

    async def text(self, encoding: str = "utf-8") -> str:
        return (await self.read()).decode(encoding)

    async def aclose(self) -> None:
        """Stops reading; the connection goes on."""
        if not self._done:
            self._done = True
            self._stream.cancel()

    async def __aenter__(self) -> Download:
        return self

    async def __aexit__(self, *_: object) -> None:
        await self.aclose()


class Blobs:
    def __init__(self, link: Link) -> None:
        self._link = link

    def bucket(self, name: str, /, *, default_ttl: Duration | None = None, max_size: int | None = None) -> BlobBucket:
        check_name(name, "bucket")
        open_body = BlobsBucket.encode(
            name=name,
            default_ttl=None if default_ttl is None else ms(default_ttl),
            max_size=max_size,
        )
        return BlobBucket(link=self._link, name=name, open_body=open_body, owners=())


@dataclass(frozen=True)
class BlobBucket:
    """A bucket's objects under one folder of owners, the root when there are none."""

    link: Link
    name: str
    open_body: bytes
    owners: tuple[str | bytes, ...]

    def of(self, *owners: Key) -> BlobBucket:
        """The folder below this one that the owners name, a segment each: of("users", 7)."""
        return BlobBucket(
            self.link,
            self.name,
            self.open_body,
            (*self.owners, *(str(o) if isinstance(o, int) else o for o in owners)),
        )

    async def _body(self, connection: Connection, **fields: Any) -> bytes:
        handle = await handle_on(connection, METHODS["blobs.open"], self.open_body)
        return BlobsCall.encode(handle=handle, owners=list(self.owners) or None, **fields)

    async def _one(self, method: str, idempotence: Any, **fields: Any) -> dict[str, Any]:
        async def attempt(connection: Connection) -> bytes:
            return await connection.session.call(METHODS[method], await self._body(connection, **fields))

        body = await self.link.run(idempotence, attempt)
        return BlobsObject.decode(body) if method in ("blobs.stat", "blobs.copy", "blobs.move") else {"body": body}

    async def stat(self, key: str) -> BlobObject | None:
        o = await self._one("blobs.stat", "read", key=key)
        return _object(o) if o.get("found") else None

    async def delete(self, key: str, *, if_match: str | None = None) -> None:
        await self._one("blobs.delete", "write", key=key, if_match=if_match)

    async def copy(
        self,
        source: str,
        to: str,
        *,
        content_type: str | None = None,
        meta: dict[str, str] | None = None,
        ttl: Duration | None = None,
        expire_at: datetime | None = None,
        if_match: str | None = None,
        if_none_match: bool = False,
    ) -> BlobObject:
        """Writes to a key an object naming the source's bytes, which are neither read nor written."""
        fields = _write_fields(content_type, meta, ttl, expire_at, if_match, if_none_match)
        return _object(await self._one("blobs.copy", "write", key=source, to=to, **fields))

    async def move(
        self,
        source: str,
        to: str,
        *,
        content_type: str | None = None,
        meta: dict[str, str] | None = None,
        ttl: Duration | None = None,
        expire_at: datetime | None = None,
        if_match: str | None = None,
        if_none_match: bool = False,
    ) -> BlobObject:
        """Copy and the source's removal in one write."""
        fields = _write_fields(content_type, meta, ttl, expire_at, if_match, if_none_match)
        return _object(await self._one("blobs.move", "write", key=source, to=to, **fields))

    async def usage(self) -> Usage:
        total = BlobsTotal.decode((await self._one("blobs.usage", "read"))["body"])
        return Usage(total.get("objects", 0), total.get("bytes", 0))

    async def clear(self) -> None:
        """Removes this folder's objects and every folder's under it, at once however many."""
        await self._one("blobs.clear", "write")

    async def scan(
        self, *, prefix: str | None = None, after: str | None = None, limit: int | None = None
    ) -> tuple[list[BlobObject], str | None]:
        """A page of the objects under this folder in the byte order of their paths, and where the next begins."""

        async def attempt(connection: Connection) -> tuple[list[bytes], bytes]:
            got = await download(
                connection,
                METHODS["blobs.scan"],
                await self._body(connection, prefix=prefix, after=after, limit=limit),
            )
            return got.items, got.trailer

        items, trailer = await self.link.run("read", attempt)
        page = BlobsPage.decode(trailer)
        return [_object(BlobsObject.decode(i)) for i in items], page.get("after", "") if page.get("more") else None

    async def all(self, *, prefix: str | None = None, limit: int | None = None) -> AsyncIterator[BlobObject]:
        after: str | None = None
        while True:
            objects, after = await self.scan(prefix=prefix, after=after, limit=limit)
            for o in objects:
                yield o
            if after is None:
                return

    async def put(
        self,
        key: str,
        body: Body,
        *,
        content_type: str | None = None,
        meta: dict[str, str] | None = None,
        ttl: Duration | None = None,
        expire_at: datetime | None = None,
        size: int | None = None,
        if_match: str | None = None,
        if_none_match: bool = False,
    ) -> BlobObject:
        """Writes an object, returning once it is durable; an upload that does not commit leaves nothing."""
        chunks, known, replayable = _source(body)
        fields = _write_fields(content_type, meta, ttl, expire_at, if_match, if_none_match)

        async def attempt(connection: Connection) -> BlobObject:
            request = await self._body(connection, key=key, size=size if size is not None else known, **fields)
            return await _upload(connection, request, chunks(), replayable)

        return await self.link.run("write", attempt)

    async def get(self, key: str, *, offset: int | None = None, length: int | None = None) -> Download | None:
        """An object being read, None when the key holds none; a range is not checked against the hash."""

        async def attempt(connection: Connection) -> Download | None:
            request = await self._body(connection, key=key, offset=offset, length=length)
            stream = await connection.session.open(METHODS["blobs.get"], request, True)
            head = await stream.next()
            o = BlobsObject.decode(head.body)
            if head.end or not o.get("found"):
                return None
            return Download(_object(o), stream)

        return await self.link.run("read", attempt)


def _source(body: Body) -> tuple[Any, int | None, bool]:
    if isinstance(body, str):
        body = body.encode()
    if isinstance(body, bytes | bytearray | memoryview):
        data = bytes(body)

        async def sliced() -> AsyncIterator[bytes]:
            for at in range(0, len(data), _CHUNK):
                yield data[at : at + _CHUNK]

        return sliced, len(data), True
    if isinstance(body, os.PathLike):
        path = Path(body)

        async def read_file() -> AsyncIterator[bytes]:
            file = await asyncio.to_thread(path.open, "rb")
            try:
                while chunk := await asyncio.to_thread(file.read, _CHUNK):
                    yield chunk
            finally:
                file.close()

        return read_file, path.stat().st_size, True
    stream = body

    async def rechunked() -> AsyncIterator[bytes]:
        if hasattr(stream, "__aiter__"):
            async for piece in typing.cast("AsyncIterable[bytes]", stream):
                for at in range(0, len(piece), _CHUNK):
                    yield piece[at : at + _CHUNK]
        elif hasattr(stream, "read"):
            while chunk := await asyncio.to_thread(stream.read, _CHUNK):  # type: ignore[union-attr]
                yield chunk
        else:
            for piece in typing.cast("Iterable[bytes]", stream):
                for at in range(0, len(piece), _CHUNK):
                    yield piece[at : at + _CHUNK]

    return rechunked, None, False


async def _upload(connection: Connection, request: bytes, chunks: AsyncIterator[bytes], replayable: bool) -> BlobObject:
    """Sends a put's REQUEST, its bytes as DATA, the DATA that ends them, and waits for the commit's object."""
    stream = await connection.session.open(METHODS["blobs.put"], request, False)
    ended = False
    try:
        last: bytes | None = None
        async for piece in chunks:
            if not piece:
                continue
            if last is not None:
                await stream.send(last, False)
            last = piece
        ended = True
        await stream.send(last or b"", True)
        return _object(BlobsObject.decode((await stream.next()).body))
    except LostError as err:
        if ended:
            raise OutcomeUnknownError(f"the connection was lost as the upload committed: {err}") from err
        if not replayable:
            raise ClosedError(f"the connection was lost during the upload, which left nothing: {err}") from err
        raise LostError(str(err), sent=False) from err
    except BaseException:
        stream.cancel()
        raise
