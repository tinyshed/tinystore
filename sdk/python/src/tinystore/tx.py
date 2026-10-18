"""A transaction of the store's own files, which never holds the writer while the program awaits.

A read goes at once and is kept with what it found; a write waits, and when
the function returns the writes commit in one call, after a check that every
key it read is still as it read it. A check that fails runs the function
again, five times at most::

    async def sell(tx: tinystore.Tx) -> None:
        left = await tx.with_(stock).get(sku) or 0
        if left < 1:
            raise SoldOut
        await tx.with_(stock).set(sku, left - 1)

    await store.tx(sell)
"""

from __future__ import annotations

import asyncio
import contextvars
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING, Any, Literal, Unpack, overload

from ._handles import Home, handle_on, key_text
from ._time import Duration, ms
from ._wire.protocol import KvCall, KvEntry, KvTx, methods
from .channels import Channel, TxChannel
from .errors import ConflictError, InvalidError
from .jobs import JobOp, Queue, StoreTxQueue, commit_jobs
from .kv import Bucket, Counters, Entry, IfVersion, Parts, below, entry_of, under_field, write_fields

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

    from ._connection import Connection, Link
    from ._values import Values
    from ._wire.codec import Key

RUNS = 5
"""How many times tx runs its function before a key it read that keeps changing is ConflictError."""

_inside: contextvars.ContextVar[bool] = contextvars.ContextVar("tinystore store tx", default=False)


def inside_store_tx() -> bool:
    """Whether the caller runs inside the function of a store's transaction: a lock's hold refuses to wait there."""
    return _inside.get()


async def run_tx[R](link: Link, fn: Callable[[Tx], Awaitable[R]]) -> R:
    """Runs fn as one transaction and gives back what it returns: its writes all commit or none do."""
    run = 1
    while True:
        tx = Tx(link)
        token = _inside.set(True)
        try:
            returned = await fn(tx)
        finally:
            _inside.reset(token)
        try:
            await tx.commit()
        except ConflictError as err:
            if run >= RUNS or "check" not in err.what:
                raise
            run += 1
            continue
        return returned


_ABSENT: dict[str, Any] = {"found": False}


@dataclass(slots=True)
class _Op:
    method: str
    parts: Parts
    fields: dict[str, Any]


class Tx:
    """The calls of a transaction, each through ``tx.with_(handle)``."""

    def __init__(self, link: Link) -> None:
        self._link = link
        self._reads: dict[tuple[Any, ...], asyncio.Future[tuple[Parts, str, dict[str, Any]]]] = {}
        self._written: dict[tuple[Any, ...], tuple[dict[str, Any], int]] = {}
        self._cleared: list[tuple[tuple[str, bytes], tuple[str, ...], int]] = []
        self._ops: list[_Op] = []
        self._extra: list[Callable[[], Awaitable[None]]] = []
        self._jobs: list[JobOp] = []
        self._file: Literal["kv.db", "jobs.db"] | None = None
        self._done = False

    @overload
    def with_[T](self, handle: Bucket[T]) -> TxBucket[T]: ...
    @overload
    def with_(self, handle: Counters) -> TxCounters: ...
    @overload
    def with_[T](self, handle: Queue[T]) -> StoreTxQueue[T]: ...
    @overload
    def with_[T](self, handle: Channel[T]) -> TxChannel[T]: ...
    def with_(
        self, handle: Bucket[Any] | Counters | Queue[Any] | Channel[Any]
    ) -> TxBucket[Any] | TxCounters | StoreTxQueue[Any] | TxChannel[Any]:
        """Takes a handle into the transaction: its calls then go through it."""
        if isinstance(handle, Channel):
            # a store's transaction commits in one kv.tx, which carries no
            # message: its messages leave once its commit answered
            return TxChannel(handle.name, handle.branch, self._publish_after)
        if isinstance(handle, Queue):
            source = handle.source
            self._takes(source.link, source.home, "jobs.db", source.describe)
            return StoreTxQueue(handle, self._keep_job)
        if isinstance(handle, Bucket):
            self.takes(handle.parts, "kv.db", f"kv bucket {handle.name}")
            return TxBucket(self, handle.parts)
        if isinstance(handle, Counters):  # pyright: ignore[reportUnnecessaryIsInstance]
            self.takes(handle.parts, "kv.db", f"kv counters {handle.name}")
            return TxCounters(self, handle.parts)
        raise InvalidError(f"tx.with_ takes a bucket, counters or a queue, not a {type(handle).__name__}")

    async def _publish_after(self, body: bytes) -> None:
        async def publish() -> None:
            await self._link.run("write", lambda c: c.session.call(methods["channels.publish"], body))

        self.after(publish)

    def takes(self, parts: Parts, file: Literal["kv.db", "jobs.db"], what: str) -> None:
        self._takes(parts.link, parts.home, file, what)

    def _keep_job(self, op: JobOp) -> None:
        self._open()
        self._jobs.append(op)

    def _takes(self, link: Link, home: Home | None, file: Literal["kv.db", "jobs.db"], what: str) -> None:
        """Refuses a handle of another store, one kept in a database's file, and one of the store's other file."""
        if link is not self._link:
            raise InvalidError(f"{what}: a handle of another store, in a transaction of this one")
        if home is not None:
            raise InvalidError(
                f"{home.describe}: {what}: kept in the database's file, outside this transaction of the store: "
                "take it in with db.tx"
            )
        if self._file is not None and self._file != file:
            raise InvalidError(
                f"{what}: kept in {file}, outside this transaction of {self._file}: open both from a database "
                "to commit them together"
            )
        self._file = file

    async def read(self, parts: Parts, key: Key) -> dict[str, Any]:
        """What the key holds as this transaction sees it: its own writes over what the store held."""
        self._open()
        text = key_text(key)
        own = self._own(parts, text)
        if own is not None:
            return own
        place = _place(parts, text)
        reading = self._reads.get(place)
        if reading is None:
            reading = asyncio.ensure_future(self._fetch(parts, text))
            # a read the function never awaited still fails the commit, which awaits it
            reading.add_done_callback(lambda f: f.cancelled() or f.exception())
            self._reads[place] = reading
        return (await asyncio.shield(reading))[2]

    def write(self, method: str, parts: Parts, fields: dict[str, Any], found: dict[str, Any] | None = None) -> None:
        """Keeps a write for the commit; found is what a read of its key answers after it."""
        self._open()
        self._ops.append(_Op(method, parts, fields))
        if found is not None and "key" in fields:
            self._written[_place(parts, key_text(fields["key"]))] = (found, len(self._ops))

    def clear(self, parts: Parts) -> None:
        self.write("kv.clear", parts, {})
        self._cleared.append((_handle_of(parts), parts.under, len(self._ops)))

    def after(self, fn: Callable[[], Awaitable[None]]) -> None:
        """What the commit does once it answered: a message published inside is handed over then."""
        self._open()
        self._extra.append(fn)

    async def commit(self) -> None:
        """Commits the writes, then does what waited for the commit."""
        self._done = True
        await self._commit_writes()
        for fn in self._extra:
            await fn()

    async def _commit_writes(self) -> None:
        if self._jobs:
            await commit_jobs(self._link, self._jobs)
            return
        reads = [await reading for reading in self._reads.values()]
        if not reads and not self._ops:
            return

        async def attempt(connection: Connection) -> bytes:
            async def handle(parts: Parts) -> int:
                return await handle_on(connection, methods[parts.open_method], parts.opener)

            checks: list[dict[str, Any]] = []
            for parts, key, found in reads:
                version = found.get("version") if found.get("found") else None
                checks.append(
                    {"handle": await handle(parts), "under": under_field(parts.under), "key": key, "version": version}
                )
            writes: list[dict[str, Any]] = []
            for op in self._ops:
                call = {**op.fields, "handle": await handle(op.parts), "under": under_field(op.parts.under)}
                writes.append({"method": methods[op.method], "call": call})
            return await connection.session.call(methods["kv.tx"], KvTx.encode(checks=checks, writes=writes))

        await self._link.run("write" if self._ops else "read", attempt)

    def _open(self) -> None:
        if self._done:
            raise InvalidError("a transaction takes its calls while its function runs, before it commits")

    def _own(self, parts: Parts, key: str) -> dict[str, Any] | None:
        # the transaction's own word on a key: a write of it, or a clear of a
        # branch above it, whichever came last
        written = self._written.get(_place(parts, key))
        cleared = 0
        for handle, under, at in self._cleared:
            if handle == _handle_of(parts) and parts.under[: len(under)] == under:
                cleared = max(cleared, at)
        if written is not None and written[1] > cleared:
            return written[0]
        return _ABSENT if cleared > 0 else None

    async def _fetch(self, parts: Parts, key: str) -> tuple[Parts, str, dict[str, Any]]:
        async def attempt(connection: Connection) -> dict[str, Any]:
            handle = await handle_on(connection, methods[parts.open_method], parts.opener)
            call = KvCall.encode(handle=handle, under=under_field(parts.under), key=key)
            return KvEntry.decode(await connection.session.call(methods["kv.get"], call))

        return parts, key, await self._link.run("read", attempt)


def _handle_of(parts: Parts) -> tuple[str, bytes]:
    if not isinstance(parts.opener, bytes):
        raise InvalidError(f"kv {parts.name}: a handle of a database, in a transaction of kv.db")
    return parts.open_method, parts.opener


def _place(parts: Parts, key: str) -> tuple[Any, ...]:
    """A key's place: its handle, its owners and itself, each kept apart."""
    return (*_handle_of(parts), parts.under, key)


class TxBucket[T]:
    """A bucket's calls inside a transaction: reads answer at once, writes wait for its commit."""

    def __init__(self, tx: Tx, parts: Parts) -> None:
        self._tx = tx
        self._parts = parts
        assert parts.values is not None
        self._values: Values[T] = parts.values

    def under(self, *owners: Key) -> TxBucket[T]:
        return TxBucket(self._tx, below(self._parts, owners))

    async def get(self, key: Key) -> T | None:
        found = await self.entry(key)
        return None if found is None else found.value

    async def entry(self, key: Key) -> Entry[T] | None:
        found = await self._tx.read(self._parts, key)
        return entry_of(key, found, self._values) if found.get("found") else None

    async def has(self, key: Key) -> bool:
        return bool((await self._tx.read(self._parts, key)).get("found"))

    async def set(
        self,
        key: Key,
        value: T,
        *,
        ttl: Duration | None = None,
        expires_at: datetime | None = None,
        **check: Unpack[IfVersion],
    ) -> None:
        raw = self._values.encode(value)
        fields = {"key": key, "value": raw, **write_fields(ttl, expires_at, check)}
        self._tx.write("kv.set", self._parts, fields, {"found": True, "value": raw})

    async def create(
        self, key: Key, value: T, *, ttl: Duration | None = None, expires_at: datetime | None = None
    ) -> bool:
        """Writes the value only where no live key is, and says whether it will."""
        if (await self._tx.read(self._parts, key)).get("found"):
            return False
        raw = self._values.encode(value)
        fields = {"key": key, "value": raw, **write_fields(ttl, expires_at)}
        self._tx.write("kv.create", self._parts, fields, {"found": True, "value": raw})
        return True

    async def add(self, key: Key, *, ttl: Duration | None = None, expires_at: datetime | None = None) -> bool:
        if (await self._tx.read(self._parts, key)).get("found"):
            return False
        self._tx.write("kv.create", self._parts, {"key": key, **write_fields(ttl, expires_at)}, {"found": True})
        return True

    async def take(self, key: Key, **check: Unpack[IfVersion]) -> T | None:
        found = await self._remove("kv.take", key, check)
        return self._values.decode(found.get("value")) if found.get("found") else None

    async def delete(self, key: Key, **check: Unpack[IfVersion]) -> bool:
        return bool((await self._remove("kv.delete", key, check)).get("found"))

    async def expire(self, key: Key, when: Duration | datetime, **check: Unpack[IfVersion]) -> bool:
        found = await self._tx.read(self._parts, key)
        if not found.get("found") and "if_version" not in check:
            return False
        if isinstance(when, datetime):
            fields = {"key": key, **write_fields(expires_at=when), **write_fields(check=check)}
            expires_at = when
        else:
            fields = {"key": key, **write_fields(ttl=when), **write_fields(check=check)}
            expires_at = datetime.now(UTC) + timedelta(milliseconds=ms(when))
        now = {**found, "expires_at": int(expires_at.timestamp() * 1000)} if found.get("found") else None
        self._tx.write("kv.expire", self._parts, fields, now)
        return bool(found.get("found"))

    async def clear(self) -> None:
        """Removes this branch's keys and every branch's under it, with the commit."""
        self._tx.clear(self._parts)

    async def _remove(self, method: str, key: Key, check: IfVersion) -> dict[str, Any]:
        # a take or a delete: what the key held, and its removal, sent when the
        # key was there or when the caller named the version it must be at
        found = await self._tx.read(self._parts, key)
        if found.get("found") or "if_version" in check:
            self._tx.write(method, self._parts, {"key": key, **write_fields(check=check)}, _ABSENT)
        return found


class TxCounters:
    """Counters inside a transaction: an add commits with its writes."""

    def __init__(self, tx: Tx, parts: Parts) -> None:
        self._tx = tx
        self._parts = parts

    def under(self, *owners: Key) -> TxCounters:
        return TxCounters(self._tx, below(self._parts, owners))

    async def add(self, key: Key, n: int = 1) -> None:
        """Adds n, 1 unless given, with the commit; the count is known once it has."""
        self._tx.write("kv.counters.add", self._parts, {"key": key, "n": n})
