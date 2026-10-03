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
import contextlib
import struct
import typing
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Literal, overload

from ._connection import Connection, Link, check_name, download, handle_on, owner_text
from ._page import Page
from ._session import LostError
from ._time import Duration, date_of, ms, unix_ms
from ._values import from_json, to_json
from ._wire.messages import METHODS, KvBucket, KvCall, KvCalls, KvEntry, KvPage, KvResults
from .config import Config
from .errors import ConflictError, CorruptError, InvalidError, OutcomeUnknownError
from .limiter import Limiter, limiter_open
from .quota import Quota, quota_open

if TYPE_CHECKING:
    import os
    from collections.abc import AsyncIterator, Awaitable, Callable, Iterable, Mapping, Sequence
    from datetime import datetime

    from ._session import Stream
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

    def quota(self, name: str, /, **windows: str) -> Quota:
        """A quota of uses by key, its windows by name, all of them counted together or none.

        Each window starts at a key's first use after the last ended::

            ai = store.kv.quota("ai", session="100/5h", weekly="300/7d")
        """
        return Quota(self._link, name, quota_open(name, windows), ())

    def once[V](self, name: str, of: type[V], /, *, default_ttl: Duration | None = None) -> Once[V]:
        """The answers a function gives once a key, of one type, kept a day unless default_ttl says."""
        check_name(name, "once")
        open_body = KvBucket.encode(name=name, once=True, default_ttl=None if default_ttl is None else ms(default_ttl))
        return Once(self._link, name, open_body, of, ())

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

    async def tx[T](self, fn: Callable[[Tx], Awaitable[T]]) -> T:
        """Runs fn as one transaction that reads before it decides what to write, as Go's Tx does.

        No writer is held across the network: a read goes to the server at
        once, a write waits, and when fn returns the writes commit in one batch
        that first checks every key fn read is still as it read it. When another
        write changed one meanwhile, fn runs again, five times at most before
        ConflictError, so it does nothing else that must happen once. It gives
        back what fn returns::

            async def sign_in(tx: tinystore.Tx) -> int:
                user_id = await codes.with_tx(tx).take(digest(code))
                if user_id is None:
                    raise InvalidCode
                sessions.of(user_id).with_tx(tx).set(digest(token), Session(device))
                return user_id

            user_id = await store.kv.tx(sign_in)
        """
        for run in range(1, TX_RUNS + 1):
            tx = Tx(self._link)
            returned = await fn(tx)
            try:
                await tx.commit()
            except ConflictError as err:
                if run < TX_RUNS and tx.stale(err):
                    continue
                raise
            return returned
        raise AssertionError("a tx ran past its last run")


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


TX_RUNS = 5
"""How many times tx runs its function before a key it read that keeps changing is ConflictError."""

_ABSENT: dict[str, Any] = {"found": False}


@dataclass
class _Read:
    """A key a tx read: the call that read it, and what it found, which its commit checks."""

    open_body: bytes
    fields: dict[str, Any]
    entry: dict[str, Any]


class Tx:
    """A transaction of store.kv.tx.

    A read goes to the server at once and is remembered with its version, so
    that a key read again answers the same; a write waits for the commit, and a
    key read after it answers what it wrote.
    """

    def __init__(self, link: Link) -> None:
        self._link = link
        self._reads: dict[str, asyncio.Future[_Read]] = {}
        self._writes: dict[str, tuple[tuple[str, bytes, dict[str, Any]], dict[str, Any]]] = {}
        self._checks = 0
        self._done = False

    def record(
        self, method: str, open_body: bytes, fields: dict[str, Any], decode: Callable[[dict[str, Any]], Any]
    ) -> asyncio.Future[Any]:
        if self._done:
            raise InvalidError("a tx takes its calls while its function runs, before it commits")
        at = _place(open_body, fields)
        if method in ("kv.get", "kv.has"):
            return asyncio.ensure_future(self._decoded(self._read(at, open_body, fields), decode))
        if method == "kv.take":
            read = self._read(at, open_body, fields)
            self._writes[at] = (("kv.delete", open_body, fields), _ABSENT)
            return asyncio.ensure_future(self._decoded(read, decode))
        answered: asyncio.Future[Any] = asyncio.get_running_loop().create_future()
        if method == "kv.set":
            entry = {"found": True, "value": fields.get("value")}
            self._writes[at] = (("kv.set", open_body, fields), entry)
            answered.set_result(decode(entry))
            return answered
        if method == "kv.delete":
            self._writes[at] = (("kv.delete", open_body, fields), _ABSENT)
            answered.set_result(decode(_ABSENT))
            return answered
        raise InvalidError(f"{method} in a tx")

    def _read(self, at: str, open_body: bytes, fields: dict[str, Any]) -> asyncio.Future[dict[str, Any]]:
        written = self._writes.get(at)
        if written is not None:
            answered: asyncio.Future[dict[str, Any]] = asyncio.get_running_loop().create_future()
            answered.set_result(written[1])
            return answered
        reading = self._reads.get(at)
        if reading is None:
            reading = asyncio.ensure_future(
                self._fetch(open_body, {"owners": fields.get("owners"), "key": fields["key"]})
            )
            # a read fn never awaited still fails the commit, which awaits it
            reading.add_done_callback(lambda f: f.cancelled() or f.exception())
            self._reads[at] = reading
        return asyncio.ensure_future(self._entry_of(reading))

    @staticmethod
    async def _entry_of(reading: asyncio.Future[_Read]) -> dict[str, Any]:
        return (await reading).entry

    @staticmethod
    async def _decoded(entry: asyncio.Future[dict[str, Any]], decode: Callable[[dict[str, Any]], Any]) -> Any:
        return decode(await entry)

    async def _fetch(self, open_body: bytes, fields: dict[str, Any]) -> _Read:
        async def attempt(connection: Connection) -> dict[str, Any]:
            handle = await handle_on(connection, METHODS["kv.open"], open_body)
            body = KvCall.encode(handle=handle, **fields)
            return KvEntry.decode(await connection.session.call(METHODS["kv.get"], body))

        return _Read(open_body, fields, await self._link.run("read", attempt))

    async def commit(self) -> None:
        """Sends what fn wrote in one batch, after a check of each key it read.

        A key it found must still be at the version it found, and a key it
        found absent still absent. A tx that only read checks its reads from
        one snapshot.
        """
        self._done = True
        calls: list[tuple[str, bytes, dict[str, Any]]] = []
        for reading in self._reads.values():
            read = await reading
            found = read.entry.get("found")
            still = {"if_version": read.entry.get("version")} if found else {"if_absent": True}
            calls.append(("kv.get", read.open_body, {**read.fields, **still}))
        self._checks = len(calls)
        calls.extend(call for call, _ in self._writes.values())
        if not calls:
            return
        writes = bool(self._writes)

        async def run(connection: Connection) -> None:
            encoded: list[dict[str, Any]] = []
            for method, open_body, fields in calls:
                handle = await handle_on(connection, METHODS["kv.open"], open_body)
                encoded.append({"method": METHODS[method], "handle": handle, **fields})
            batch = METHODS["kv.batch" if writes else "kv.view"]
            await connection.session.call(batch, KvCalls.encode(calls=encoded))

        await self._link.run("write" if writes else "read", run)

    def stale(self, err: ConflictError) -> bool:
        """Whether the commit failed for a key fn read that changed since, which another run reads anew."""
        call = err.what.get("call", "")
        return call.isdigit() and int(call) < self._checks


def _place(open_body: bytes, fields: dict[str, Any]) -> str:
    """A key's place as one text: its bucket's open and its owners included, an integer its decimal spelling."""

    def part(p: Any) -> str:
        return f"b{p.hex()}" if isinstance(p, bytes) else f"t{p}"

    return "\0".join([open_body.hex(), *map(part, fields.get("owners") or ()), part(fields["key"])])


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

    def with_tx(self, tx: Batch | Tx) -> BucketTx[V]:
        """This bucket's calls inside a batch or a view, or inside a tx, whose reads answer at once."""
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
    """A bucket's calls inside a batch, a view or a tx; each returns a future."""

    def __init__(self, bucket: Bucket[V], tx: Batch | Tx) -> None:
        self._bucket, self._tx = bucket, tx

    def of(self, *owners: Key) -> BucketTx[V]:
        """The branch below this one that the owners name, inside the same batch, view or tx."""
        return BucketTx(self._bucket.of(*owners), self._tx)

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


class Once[V]:
    """Answers kept once a key, as Go's kv.Once keeps them: open them with `store.kv.once(name, Receipt)`.

    The server lets one call of a key run its function at a time, every client of the store included,
    and keeps what it returned, so that a request sent again is answered as the first was.
    """

    def __init__(self, link: Link, name: str, open_body: bytes, of: Any, owners: tuple[str | bytes, ...]) -> None:
        self._link, self.name, self._open, self._of, self._owners = link, name, open_body, of, owners

    def of(self, *owners: Key) -> Once[V]:
        """The answers of a branch, each key apart from the same key elsewhere."""
        return Once(self._link, self.name, self._open, self._of, (*self._owners, *map(owner_text, owners)))

    async def run(self, key: Key, fn: Callable[[], Awaitable[V]]) -> V:
        """The answer kept under key, or fn's, which is kept.

        A call of a key another call is running waits for it and gets its answer; an exception of
        fn keeps nothing and is raised, so the next call runs fn again. A connection lost after fn
        returned and before its answer was kept is OutcomeUnknownError::

            receipt = await charges.run(request_id, lambda: pay.charge(order, request_id))
        """
        stream, kept = await self._claim(key)
        if stream is None:
            return _decode(self._of, kept)
        try:
            answer = await fn()
            body = KvEntry.encode(found=True, value=_encode(self._of, answer))
        except Exception:
            with contextlib.suppress(Exception):
                await _end(stream, KvEntry.encode())
            raise
        except BaseException:
            stream.cancel()
            raise
        await _end(stream, body)
        return answer

    async def get(self, key: Key) -> V | None:
        """The answer kept under key, None when none is."""
        e = await self._call("kv.get", "read", key)
        return _decode(self._of, e.get("value")) if e.get("found") else None

    async def delete(self, key: Key) -> None:
        """Forgets the answer kept under key, so that the next run runs again."""
        await self._call("kv.delete", "write", key)

    async def _claim(self, key: Key) -> tuple[Stream | None, Raw]:
        """The answer kept under key, or the stream the server handed the run on; a lost connection asks again."""

        async def attempt(connection: Connection) -> tuple[Stream | None, Raw]:
            handle = await handle_on(connection, METHODS["kv.open"], self._open)
            body = KvCall.encode(handle=handle, owners=list(self._owners) or None, key=key)
            stream = await connection.session.open(METHODS["kv.run"], body, False)
            try:
                first = await stream.next()
            except BaseException:
                stream.cancel()
                raise
            if first.end:
                return None, KvEntry.decode(first.body).get("value")
            return stream, None

        return await self._link.run("read", attempt)

    async def _call(self, method: str, idempotence: Literal["read", "write"], key: Key) -> dict[str, Any]:
        async def attempt(connection: Connection) -> dict[str, Any]:
            handle = await handle_on(connection, METHODS["kv.open"], self._open)
            body = KvCall.encode(handle=handle, owners=list(self._owners) or None, key=key)
            return KvEntry.decode(await connection.session.call(METHODS[method], body))

        return await self._link.run(idempotence, attempt)


async def _end(stream: Stream, body: bytes) -> None:
    """Sends what a run keeps as the stream's last DATA, and waits for the server's, which comes once it is kept."""
    try:
        await stream.send(body, True)
        await stream.next()
    except LostError as lost:
        raise OutcomeUnknownError(
            f"the connection was lost before the answer was kept; the next run may run again: {lost}"
        ) from lost
