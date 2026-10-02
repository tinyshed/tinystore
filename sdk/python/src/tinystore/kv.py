"""The application's current state in kv.db, as Go's kv keeps it.

Buckets of one value type by key, keys in branches, expiry by the store's
clock, versions that never repeat. A bucket's handle opens with its first
call. A value is kept as Go's codecFor keeps its type, so that Go, Bun and
Python read each other's buckets::

    str → its UTF-8    bytes → the bytes    int, bool → an integer
    float → eight bytes, big-endian    None → nothing: a set    anything else → JSON
"""

from __future__ import annotations

import asyncio
import struct
import typing
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Literal, overload

from ._connection import Connection, Link, check_name, download, handle_on, owner_text
from ._page import Page
from ._time import Duration, date_of, ms, unix_ms
from ._values import from_json, to_json
from ._wire.messages import METHODS, KvBucket, KvCall, KvCalls, KvEntry, KvPage, KvResults
from .config import Config
from .errors import CorruptError, InvalidError
from .limiter import Limiter, limiter_open

if TYPE_CHECKING:
    import os
    from collections.abc import AsyncIterator, Callable, Iterable, Mapping, Sequence
    from datetime import datetime

    from ._wire.codec import Key, Raw

_MISSING: Any = object()
_INT64 = (-(1 << 63), (1 << 63) - 1)


@dataclass(frozen=True, slots=True)
class Entry[V]:
    value: V
    version: str
    """compared only for equality: give it back as if_version"""
    expires: datetime | None
    key: str | bytes = ""
    """a scan's entry alone"""


def _encode(of: Any, value: Any) -> Raw:
    if of is None:
        if value is not None:
            raise InvalidError("a bucket of None keeps no value: set None")
        return None
    if of is str:
        if not isinstance(value, str):
            raise InvalidError(f"a {type(value).__name__} in a bucket of str")
        return value.encode()
    if of is bytes:
        if not isinstance(value, bytes | bytearray | memoryview):
            raise InvalidError(f"a {type(value).__name__} in a bucket of bytes")
        return bytes(typing.cast("bytes", value))
    if of is bool:
        if not isinstance(value, bool):
            raise InvalidError(f"a {type(value).__name__} in a bucket of bool")
        return int(value)
    if of is int:
        if not isinstance(value, int) or isinstance(value, bool) or not _INT64[0] <= value <= _INT64[1]:
            raise InvalidError(f"{value!r} is not a 64-bit integer")
        return value
    if of is float:
        if not isinstance(value, int | float) or isinstance(value, bool):
            raise InvalidError(f"a {type(value).__name__} in a bucket of float")
        return struct.pack(">d", value)
    return to_json(value).encode()


def _decode(of: Any, raw: Raw) -> Any:
    if of is None:
        if raw is not None:
            raise CorruptError("a stored value in a bucket that keeps none")
        return None
    if of is bytes:
        if isinstance(raw, int):
            raise CorruptError("a stored integer where bytes were written")
        return raw or b""
    if of is str:
        if isinstance(raw, int):
            raise CorruptError("a stored integer where text was written")
        try:
            return (raw or b"").decode()
        except UnicodeDecodeError:
            raise CorruptError("a stored value is bytes that are not UTF-8: open the bucket of bytes") from None
    if of in (int, bool):
        if not isinstance(raw, int):
            raise CorruptError("a stored value is not the integer that was written")
        return raw != 0 if of is bool else raw
    if of is float:
        if not isinstance(raw, bytes) or len(raw) != 8:
            raise CorruptError("a stored value is not a float's eight bytes")
        return struct.unpack(">d", raw)[0]
    if not isinstance(raw, bytes):
        raise CorruptError("a stored value is not the JSON that was written")
    return from_json(raw, of)


def _write_fields(ttl: Duration | None, expire_at: datetime | None, if_version: str | None) -> dict[str, Any]:
    return {
        "ttl": None if ttl is None else ms(ttl),
        "expire_at": None if expire_at is None else unix_ms(expire_at),
        "if_version": None if if_version is None else if_version.encode("latin-1"),
    }


def _entry(of: Any, e: dict[str, Any]) -> Entry[Any]:
    version = e.get("version", b"").decode("latin-1")
    return Entry(_decode(of, e.get("value")), version, date_of(e.get("expires")), e.get("key", ""))


class Kv:
    def __init__(self, link: Link) -> None:
        self._link = link
        self._configs: list[Config[Any]] = []

    async def config[T](
        self,
        name: str,
        of: type[T],
        /,
        *,
        file: Mapping[str, Any] | None = None,
        prefix: str = "",
        env: Mapping[str, str] | bool = True,
        env_file: str | os.PathLike[str] | Sequence[str | os.PathLike[str]] | None = None,
        secret: Iterable[str] = (),
        validate: Callable[[T], object] | None = None,
    ) -> Config[T]:
        """The config name, of a type whose defaults are its own: a dataclass, or a model.

        A file's values, then the environment, then what update kept go over the defaults, each over
        the one before. It returns once the server's state is read, and follows every change from
        then on, whoever makes it, until the store closes::

            cfg = await store.kv.config("app", Settings, env_file=".env")
            cfg.value.port                     # PORT=3000 in .env makes it 3000
            await cfg.update({"port": 4000})   # kept: 4000 after a restart too
        """
        config = Config(
            self._link, name, of, file=file, prefix=prefix, env=env, env_file=env_file, secret=secret, validate=validate
        )
        self._configs.append(config)
        try:
            await config.start()
        except BaseException:
            self._configs.remove(config)
            raise
        return config

    def limiter(self, name: str, /, *, rate: str, burst: int | None = None) -> Limiter:
        """A limiter of requests by key: `store.kv.limiter("api", rate="100/s", burst=20)`."""
        return Limiter(self._link, name, limiter_open(name, rate, burst), ())

    def stop(self) -> None:
        """Stops following configs, as the store does when it closes."""
        for config in self._configs:
            config.stop()

    @overload
    def bucket(
        self, name: str, /, *, default_ttl: Duration | None = None, sliding: Duration | None = None
    ) -> Bucket[Any]: ...
    @overload
    def bucket[V](
        self,
        name: str,
        of: type[V],
        /,
        *,
        default_ttl: Duration | None = None,
        sliding: Duration | None = None,
    ) -> Bucket[V]: ...
    @overload
    def bucket(
        self,
        name: str,
        of: None,
        /,
        *,
        default_ttl: Duration | None = None,
        sliding: Duration | None = None,
    ) -> Bucket[None]: ...
    def bucket(
        self,
        name: str,
        of: Any = _MISSING,
        /,
        *,
        default_ttl: Duration | None = None,
        sliding: Duration | None = None,
    ) -> Bucket[Any]:
        """A bucket of values of one type: a type of the table above, or any JSON can hold."""
        check_name(name, "bucket")
        if default_ttl is not None and sliding is not None:
            raise InvalidError(f"bucket {name}: sliding and default_ttl are one or the other")
        open_body = KvBucket.encode(
            name=name,
            default_ttl=None if default_ttl is None else ms(default_ttl),
            sliding=None if sliding is None else ms(sliding),
        )
        return Bucket(self._link, name, open_body, Any if of is _MISSING else of, ())

    def counters(
        self,
        name: str,
        /,
        *,
        default_ttl: Duration | None = None,
        lose_at_most: Duration | None = None,
    ) -> Counters:
        """Counters: an int64 a key, 0 when absent; lose_at_most keeps changes in memory between flushes."""
        check_name(name, "counters")
        open_body = KvBucket.encode(
            name=name,
            counters=True,
            default_ttl=None if default_ttl is None else ms(default_ttl),
            lose_at_most=None if lose_at_most is None else ms(lose_at_most),
        )
        return Counters(self._link, name, open_body, ())

    def batch(self) -> Batch:
        """Calls in one transaction, all or none, sent as the async with block ends::

        async with store.kv.batch() as tx:
            sessions.with_tx(tx).set(token, session)
            codes.with_tx(tx).delete(code)
        """
        return Batch(self._link, "kv.batch")

    def view(self) -> Batch:
        """Gets and hases from one snapshot, their futures settled as the block ends."""
        return Batch(self._link, "kv.view")


@dataclass
class _Call:
    method: str
    open_body: bytes
    fields: dict[str, Any]
    decode: Callable[[dict[str, Any]], Any]
    future: asyncio.Future[Any]


class Batch:
    """The calls of a batch or a view: each returns a future that settles once the batch has."""

    def __init__(self, link: Link, method: Literal["kv.batch", "kv.view"]) -> None:
        self._link = link
        self.method = method
        self.calls: list[_Call] = []

    def record(
        self, method: str, open_body: bytes, fields: dict[str, Any], decode: Callable[[dict[str, Any]], Any]
    ) -> asyncio.Future[Any]:
        if self.method == "kv.view" and method not in ("kv.get", "kv.has"):
            raise InvalidError(f"{method} in a view, which reads")
        future: asyncio.Future[Any] = asyncio.get_running_loop().create_future()
        self.calls.append(_Call(method, open_body, fields, decode, future))
        return future

    async def __aenter__(self) -> Batch:
        return self

    async def __aexit__(self, kind: object, err: object, trace: object) -> None:
        if err is not None or not self.calls:
            for call in self.calls:
                call.future.cancel()
            return

        async def run(connection: Connection) -> list[dict[str, Any]]:
            calls: list[dict[str, Any]] = []
            for call in self.calls:
                handle = await handle_on(connection, METHODS["kv.open"], call.open_body)
                calls.append({"method": METHODS[call.method], "handle": handle, **call.fields})
            body = await connection.session.call(METHODS[self.method], KvCalls.encode(calls=calls))
            return KvResults.decode(body).get("entries", [])

        try:
            results = await self._link.run("read" if self.method == "kv.view" else "write", run)
        except BaseException as failure:
            for call in self.calls:
                call.future.set_exception(failure)
                call.future.exception()
            raise
        for call, result in zip(self.calls, results, strict=False):
            try:
                call.future.set_result(call.decode(result))
            except Exception as failure:
                call.future.set_exception(failure)


class Bucket[V]:
    """A bucket's values under one branch of owners: sessions.of(user.id).get(token)."""

    def __init__(self, link: Link, name: str, open_body: bytes, of: Any, owners: tuple[str | bytes, ...]) -> None:
        self._link, self.name, self.open_body, self.value_type, self.owners = (
            link,
            name,
            open_body,
            of,
            owners,
        )

    def of(self, *owners: Key) -> Bucket[V]:
        """The branch below this one that the owners name."""
        return Bucket(self._link, self.name, self.open_body, self.value_type, (*self.owners, *map(owner_text, owners)))

    def with_tx(self, tx: Batch) -> BucketTx[V]:
        """This bucket's calls inside a batch or a view."""
        return BucketTx(self, tx)

    async def _call(self, method: str, idempotence: Literal["read", "write"], **fields: Any) -> dict[str, Any]:
        async def attempt(connection: Connection) -> dict[str, Any]:
            handle = await handle_on(connection, METHODS["kv.open"], self.open_body)
            body = KvCall.encode(handle=handle, owners=list(self.owners) or None, **fields)
            return KvEntry.decode(await connection.session.call(METHODS[method], body))

        return await self._link.run(idempotence, attempt)

    async def get(self, key: Key) -> V | None:
        """The key's value, None when it holds none."""
        entry = await self.get_entry(key)
        return None if entry is None else entry.value

    async def get_entry(self, key: Key) -> Entry[V] | None:
        e = await self._call("kv.get", "read", key=key)
        return _entry(self.value_type, e) if e.get("found") else None

    async def has(self, key: Key) -> bool:
        return bool((await self._call("kv.has", "read", key=key)).get("found"))

    async def set(
        self,
        key: Key,
        value: V,
        *,
        ttl: Duration | None = None,
        expire_at: datetime | None = None,
        if_version: str | None = None,
    ) -> None:
        """Writes the value; a live key keeps its expiry unless one is given."""
        await self.set_entry(key, value, ttl=ttl, expire_at=expire_at, if_version=if_version)

    async def set_entry(
        self,
        key: Key,
        value: V,
        *,
        ttl: Duration | None = None,
        expire_at: datetime | None = None,
        if_version: str | None = None,
    ) -> Entry[V]:
        raw = _encode(self.value_type, value)
        e = await self._call("kv.set", "write", key=key, value=raw, **_write_fields(ttl, expire_at, if_version))
        return Entry(value, e.get("version", b"").decode("latin-1"), date_of(e.get("expires")))

    async def set_if_absent(
        self, key: Key, value: V, *, ttl: Duration | None = None, expire_at: datetime | None = None
    ) -> bool:
        """Writes the value only where no live key is, and says whether it did."""
        created, _ = await self.set_entry_if_absent(key, value, ttl=ttl, expire_at=expire_at)
        return created

    async def set_entry_if_absent(
        self, key: Key, value: V, *, ttl: Duration | None = None, expire_at: datetime | None = None
    ) -> tuple[bool, Entry[V]]:
        """Writes only where no live key is: whether it did, and the new key's entry or the live one's."""
        raw = _encode(self.value_type, value)
        e = await self._call(
            "kv.set",
            "write",
            key=key,
            value=raw,
            if_absent=True,
            **_write_fields(ttl, expire_at, None),
        )
        created = bool(e.get("found"))
        entry = (
            _entry(self.value_type, e)
            if not created
            else Entry(value, e.get("version", b"").decode("latin-1"), date_of(e.get("expires")))
        )
        return created, entry

    async def delete(self, key: Key, *, if_version: str | None = None) -> None:
        await self._call("kv.delete", "write", key=key, **_write_fields(None, None, if_version))

    async def take(self, key: Key, *, if_version: str | None = None) -> V | None:
        """Reads the value and deletes the key in one write: a one-time code read and burned."""
        e = await self._call("kv.take", "write", key=key, **_write_fields(None, None, if_version))
        return _decode(self.value_type, e.get("value")) if e.get("found") else None

    async def touch(
        self,
        key: Key,
        *,
        ttl: Duration | None = None,
        expire_at: datetime | None = None,
        if_version: str | None = None,
    ) -> bool:
        """Gives a live key a new expiry, keeping its value and version; False when it holds none."""
        e = await self._call("kv.touch", "write", key=key, **_write_fields(ttl, expire_at, if_version))
        return bool(e.get("found"))

    async def clear(self) -> None:
        """Removes this branch's keys and every branch under it, at once however many."""
        await self._call("kv.clear", "write")

    async def scan(self, *, after: Key | None = None, limit: int | None = None) -> Page[Entry[V], str | bytes]:
        """One page of this branch's own keys in the byte order of their text, and the key the next begins after."""

        async def attempt(connection: Connection) -> tuple[list[bytes], bytes]:
            handle = await handle_on(connection, METHODS["kv.open"], self.open_body)
            body = KvCall.encode(handle=handle, owners=list(self.owners) or None, after=after, limit=limit)
            got = await download(connection, METHODS["kv.scan"], body)
            return got.items, got.trailer

        items, trailer = await self._link.run("read", attempt)
        page = KvPage.decode(trailer)
        entries = [_entry(self.value_type, KvEntry.decode(item)) for item in items]
        return Page(entries, page.get("after", "") if page.get("more") else None)

    async def all(self, *, limit: int | None = None) -> AsyncIterator[Entry[V]]:
        """Walks this branch's own keys a page at a time, holding no snapshot between pages."""
        after: Key | None = None
        while True:
            entries, after = await self.scan(after=after, limit=limit)
            for entry in entries:
                yield entry
            if after is None:
                return


class BucketTx[V]:
    """A bucket's calls inside a batch or a view; each returns a future."""

    def __init__(self, bucket: Bucket[V], tx: Batch) -> None:
        self._bucket, self._tx = bucket, tx

    def _record(self, method: str, decode: Callable[[dict[str, Any]], Any], **fields: Any) -> asyncio.Future[Any]:
        b = self._bucket
        return self._tx.record(method, b.open_body, {"owners": list(b.owners) or None, **fields}, decode)

    def get(self, key: Key) -> asyncio.Future[V | None]:
        of = self._bucket.value_type
        return self._record("kv.get", lambda e: _decode(of, e.get("value")) if e.get("found") else None, key=key)

    def has(self, key: Key) -> asyncio.Future[bool]:
        return self._record("kv.has", lambda e: bool(e.get("found")), key=key)

    def set(
        self,
        key: Key,
        value: V,
        *,
        ttl: Duration | None = None,
        expire_at: datetime | None = None,
        if_version: str | None = None,
    ) -> asyncio.Future[None]:
        raw = _encode(self._bucket.value_type, value)
        return self._record(
            "kv.set",
            lambda _: None,
            key=key,
            value=raw,
            **_write_fields(ttl, expire_at, if_version),
        )

    def delete(self, key: Key, *, if_version: str | None = None) -> asyncio.Future[None]:
        return self._record("kv.delete", lambda _: None, key=key, **_write_fields(None, None, if_version))

    def take(self, key: Key) -> asyncio.Future[V | None]:
        of = self._bucket.value_type
        return self._record("kv.take", lambda e: _decode(of, e.get("value")) if e.get("found") else None, key=key)


class Counters:
    """Counters under one branch: an int64 a key, 0 when absent; a sum past int64 is refused."""

    def __init__(self, link: Link, name: str, open_body: bytes, owners: tuple[str | bytes, ...]) -> None:
        self._link, self.name, self._open, self._owners = link, name, open_body, owners

    def of(self, *owners: Key) -> Counters:
        return Counters(self._link, self.name, self._open, (*self._owners, *map(owner_text, owners)))

    async def _call(self, method: str, key: Key | None, n: int | None = None) -> int:
        async def attempt(connection: Connection) -> dict[str, Any]:
            handle = await handle_on(connection, METHODS["kv.open"], self._open)
            body = KvCall.encode(handle=handle, owners=list(self._owners) or None, key=key, n=n)
            return KvEntry.decode(await connection.session.call(METHODS[method], body))

        e = await self._link.run("read" if method == "kv.get" else "write", attempt)
        value = e.get("value")
        return value if isinstance(value, int) else 0

    async def get(self, key: Key) -> int:
        return await self._call("kv.get", key)

    async def add(self, key: Key, n: int = 1) -> int:
        """Adds n and gives the new value."""
        return await self._call("kv.add", key, n)

    async def max(self, key: Key, n: int) -> int:
        """Keeps the larger of the counter and n, and gives it."""
        return await self._call("kv.max", key, n)

    async def delete(self, key: Key) -> None:
        await self._call("kv.delete", key)

    async def clear(self) -> None:
        await self._call("kv.clear", None)
