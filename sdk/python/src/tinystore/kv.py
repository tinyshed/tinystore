"""The application's current state, as plan/api/kv.md has it.

Buckets of one value type by key, keys in branches, expiry by the store's
clock and versions that never repeat, with counters beside them. A handle
opens with its first call on a connection::

    sessions = store.bucket("sessions", Session, idle="30d")
    await sessions.under(user.id).set(digest(token), session)
    session = await sessions.under(user.id).get(digest(token))   # Session | None
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from typing import TYPE_CHECKING, Any, NotRequired, TypedDict, Unpack

from ._connection import check_name
from ._handles import Home, Opener, Via, key_text, via_link
from ._time import Duration, date_of, ms, unix_ms
from ._wire.protocol import (
    KvBranch,
    KvBucketOpen,
    KvCall,
    KvCount,
    KvCountersOpen,
    KvEntry,
    KvFound,
    KvList,
    KvPage,
    KvWritten,
    methods,
)
from .errors import InvalidError

if TYPE_CHECKING:
    from collections.abc import AsyncIterator

    from ._connection import Idempotence, Link
    from ._values import Values
    from ._wire.codec import Key

Version = bytes
"""What a key was when it was read: compared only for equality, given back to ``if_version``."""


class IfVersion(TypedDict):
    """Writes only while the key is at this version: ConflictError otherwise.

    It takes a version and never None, so that an absent key never writes
    without its check.
    """

    if_version: NotRequired[Version]


@dataclass(frozen=True, slots=True)
class Entry[T]:
    """A key's value with its version and expiry."""

    key: str
    value: T
    version: Version
    """compared only for equality: give it back to if_version"""
    expires_at: datetime | None
    """None for a key that never expires"""


@dataclass(frozen=True, slots=True)
class Page[T]:
    """A page of a branch's keys, and where the next starts when there is one."""

    entries: list[Entry[T]]
    next: str | None


@dataclass(frozen=True, slots=True)
class Parts:
    """What a transaction needs of a handle it takes in."""

    link: Link
    name: str
    opener: Opener
    open_method: str
    under: tuple[str, ...]
    values: Values[Any] | None
    """a bucket's values; None for counters"""
    home: Home | None
    """the database whose file keeps the handle; None for the store's own"""


def write_fields(
    ttl: Duration | None = None, expires_at: datetime | None = None, check: IfVersion | None = None
) -> dict[str, Any]:
    """A write's expiry and condition as a call carries them."""
    fields: dict[str, Any] = {}
    if ttl is not None and expires_at is not None:
        raise InvalidError("a write gives ttl or expires_at, not both")
    if ttl is not None:
        fields["ttl"] = ms(ttl, "a key's ttl")
    if expires_at is not None:
        fields["expires_at"] = unix_ms(expires_at, "a key's expiry")
    if check is not None and "if_version" in check:
        version = check["if_version"]
        if not isinstance(version, bytes):  # pyright: ignore[reportUnnecessaryIsInstance]
            raise InvalidError(f"if_version={version!r}: give the version an entry has, never None")
        fields["if_version"] = version
    return fields


def entry_of[T](key: Key, found: dict[str, Any], values: Values[T]) -> Entry[T]:
    return Entry(
        key=key_text(key),
        value=values.decode(found.get("value")),
        version=found.get("version", b""),
        expires_at=date_of(found.get("expires_at")),
    )


def under_field(under: tuple[str, ...]) -> list[str] | None:
    """A branch's owners as a call sends them: none at the root."""
    return list(under) if under else None


def bucket_open(name: str, ttl: Duration | None, idle: Duration | None, encrypted: bool, database: int | None) -> bytes:
    """What a bucket's open sends, checked before anything leaves."""
    check_name(name, "bucket")
    if ttl is not None and idle is not None:
        raise InvalidError(f"kv bucket {name}: ttl and idle are one or the other")
    return KvBucketOpen.encode(
        name=name,
        ttl=None if ttl is None else ms(ttl, f"kv bucket {name}: ttl"),
        idle=None if idle is None else ms(idle, f"kv bucket {name}: idle"),
        database=database,
        encrypted=encrypted or None,
    )


class Bucket[T]:
    """A bucket's values under one branch of owners, the bucket's own when there are none.

    Buckets come from ``store.bucket``, ``db.bucket`` and their ``under``;
    inside a transaction, from ``tx.with_(bucket)``, whose calls go through
    the transaction.
    """

    def __init__(self, parts: Parts, via: Via | None = None) -> None:
        self.name = parts.name
        self._parts = parts
        self._via = via or via_link(parts.link)
        assert parts.values is not None
        self._values: Values[T] = parts.values

    @property
    def parts(self) -> Parts:
        return self._parts

    def under(self, *owners: Key) -> Bucket[T]:
        """The branch the owners name below this one: whose keys these are."""
        parts = below(self._parts, owners)
        return Bucket(parts, self._via)

    async def get(self, key: Key) -> T | None:
        """The key's value, None when it holds none."""
        found = KvEntry.decode(await self._call("kv.get", {"key": key}, "read"))
        return self._values.decode(found.get("value")) if found.get("found") else None

    async def entry(self, key: Key) -> Entry[T] | None:
        """The key's value with its version and expiry, None when it holds none."""
        found = KvEntry.decode(await self._call("kv.get", {"key": key}, "read"))
        return entry_of(key, found, self._values) if found.get("found") else None

    async def has(self, key: Key) -> bool:
        return KvFound.decode(await self._call("kv.has", {"key": key}, "read")).get("found", False)

    async def set(
        self,
        key: Key,
        value: T,
        *,
        ttl: Duration | None = None,
        expires_at: datetime | None = None,
        **check: Unpack[IfVersion],
    ) -> None:
        """Writes the value; a key that exists keeps its expiry unless the call gives one."""
        fields = {"key": key, "value": self._values.encode(value), **write_fields(ttl, expires_at, check)}
        await self._call("kv.set", fields, "write")

    async def create(
        self, key: Key, value: T, *, ttl: Duration | None = None, expires_at: datetime | None = None
    ) -> bool:
        """Writes the value only where no live key is, and says whether it did."""
        fields = {"key": key, "value": self._values.encode(value), **write_fields(ttl, expires_at)}
        return KvWritten.decode(await self._call("kv.create", fields, "write")).get("written", False)

    async def add(self, key: Key, *, ttl: Duration | None = None, expires_at: datetime | None = None) -> bool:
        """Puts the key in a bucket used as a set; False when it was there."""
        fields = {"key": key, **write_fields(ttl, expires_at)}
        return KvWritten.decode(await self._call("kv.create", fields, "write")).get("written", False)

    async def take(self, key: Key, **check: Unpack[IfVersion]) -> T | None:
        """Reads the value and removes the key in one commit: of two callers taking a code at once, one gets it."""
        found = KvEntry.decode(await self._call("kv.take", {"key": key, **write_fields(check=check)}, "write"))
        return self._values.decode(found.get("value")) if found.get("found") else None

    async def delete(self, key: Key, **check: Unpack[IfVersion]) -> bool:
        """Removes the key, and says whether it was there."""
        body = await self._call("kv.delete", {"key": key, **write_fields(check=check)}, "write")
        return KvFound.decode(body).get("found", False)

    async def expire(self, key: Key, when: Duration | datetime, **check: Unpack[IfVersion]) -> bool:
        """Gives a live key a new expiry, a span from now or a time, keeping its value; False when it holds none."""
        expiry = write_fields(expires_at=when) if isinstance(when, datetime) else write_fields(ttl=when)
        body = await self._call("kv.expire", {"key": key, **expiry, **write_fields(check=check)}, "write")
        return KvFound.decode(body).get("found", False)

    async def clear(self) -> None:
        """Removes this branch's keys and every branch under it, at once however many."""
        under = under_field(self._parts.under)
        await self._via.call(
            methods["kv.bucket.open"],
            self._parts.opener,
            methods["kv.clear"],
            lambda handle: KvBranch.encode(handle=handle, under=under),
            "write",
        )

    async def list(self, *, limit: int | None = None, after: Key | None = None) -> Page[T]:
        """One page of this branch's own keys, in the byte order of their text: "10" before "9"."""
        under = under_field(self._parts.under)
        body = await self._via.call(
            methods["kv.bucket.open"],
            self._parts.opener,
            methods["kv.list"],
            lambda handle: KvList.encode(handle=handle, under=under, after=after, limit=limit),
            "read",
        )
        page = KvPage.decode(body)
        entries = [entry_of(found.get("key", ""), found, self._values) for found in page.get("entries", [])]
        following = page.get("next")
        return Page(entries, None if following is None else key_text(following))

    async def all(self, *, limit: int | None = None) -> AsyncIterator[Entry[T]]:
        """Walks this branch's own keys a page at a time, holding nothing between pages.

        A key written during the walk may or may not be met. It renews no idle key.
        """
        after: str | None = None
        while True:
            page = await self.list(limit=limit, after=after)
            for entry in page.entries:
                yield entry
            if page.next is None:
                return
            after = page.next

    async def _call(self, method: str, fields: dict[str, Any], idempotence: Idempotence) -> bytes:
        under = under_field(self._parts.under)
        return await self._via.call(
            methods["kv.bucket.open"],
            self._parts.opener,
            methods[method],
            lambda handle: KvCall.encode(fields, handle=handle, under=under),
            idempotence,
        )


def below(parts: Parts, owners: tuple[Key, ...]) -> Parts:
    return Parts(
        parts.link,
        parts.name,
        parts.opener,
        parts.open_method,
        (*parts.under, *(key_text(owner) for owner in owners)),
        parts.values,
        parts.home,
    )


def counters_open(name: str, ttl: Duration | None, flush_every: Duration | None) -> bytes:
    check_name(name, "counters")
    return KvCountersOpen.encode(
        name=name,
        ttl=None if ttl is None else ms(ttl, f"kv counters {name}: ttl"),
        flush_every=None if flush_every is None else ms(flush_every, f"kv counters {name}: flush_every"),
    )


class Counters:
    """Numbers by key that only add up: 0 for one never added to, or expired. They come from ``store.counters``."""

    def __init__(self, parts: Parts) -> None:
        self.name = parts.name
        self._parts = parts

    @property
    def parts(self) -> Parts:
        return self._parts

    def under(self, *owners: Key) -> Counters:
        return Counters(below(self._parts, owners))

    async def add(self, key: Key, n: int = 1) -> int:
        """Adds n, 1 unless given, and gives the new count."""
        return KvCount.decode(await self._call("kv.counters.add", {"key": key, "n": n}, "write")).get("value", 0)

    async def get(self, key: Key) -> int:
        return KvCount.decode(await self._call("kv.counters.get", {"key": key}, "read")).get("value", 0)

    async def delete(self, key: Key) -> bool:
        """Removes the counter, and says whether it was there."""
        return KvFound.decode(await self._call("kv.counters.delete", {"key": key}, "write")).get("found", False)

    async def clear(self) -> None:
        """Removes this branch's counters and every branch's under it."""
        under = under_field(self._parts.under)
        await via_link(self._parts.link).call(
            methods["kv.counters.open"],
            self._parts.opener,
            methods["kv.counters.clear"],
            lambda handle: KvBranch.encode(handle=handle, under=under),
            "write",
        )

    async def _call(self, method: str, fields: dict[str, Any], idempotence: Idempotence) -> bytes:
        return await call_on(self._parts, method, fields, idempotence)


async def call_on(parts: Parts, method: str, fields: dict[str, Any], idempotence: Idempotence) -> bytes:
    """Calls a method of a handle's branch on the link's connection, the handle opened there first."""
    under = under_field(parts.under)
    return await via_link(parts.link).call(
        methods[parts.open_method],
        parts.opener,
        methods[method],
        lambda handle: KvCall.encode(fields, handle=handle, under=under),
        idempotence,
    )
