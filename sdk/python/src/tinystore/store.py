"""The store in a directory, and how a program reaches it.

::

    async with await tinystore.open("data") as store:
        sessions = store.bucket("sessions", Session, idle="30d")
        await sessions.under(user.id).set(digest(token), session)
"""

from __future__ import annotations

import asyncio
import re
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Literal, Self, overload

from . import _runtime
from ._connection import (
    Durability,
    Link,
    Started,
    asking,
    check_durability,
    in_process,
    private_child,
    remote,
    sidecar,
)
from ._time import Duration, ms, spelled
from ._values import json_values, values_of
from .clock import Clock
from .errors import InvalidError
from .jobs import Backoff, Concurrency, Outcome, Queue, QueueOptions, Run, Schedule, Worker, open_queue, open_schedule
from .kv import Bucket, Counters, Parts, bucket_open, counters_open
from .limits import Quota, RateLimit, quota_open, rate_limit_open
from .once import Once, once_open
from .sql import Database, Migrations, open_database
from .tx import Tx, run_tx

if TYPE_CHECKING:
    import os
    import ssl
    from collections.abc import Awaitable, Callable
    from datetime import datetime
    from types import TracebackType


@dataclass(frozen=True, slots=True)
class Status:
    """What a server is, as its WELCOME said it to this store's connection."""

    server: str
    """its version: v0.1.0, or (devel) for one built from a checkout"""
    protocol: int
    engines: list[str]
    """the engines it serves"""
    capability: Literal["admin", "data", "read"]
    """admin may change a schema and drop records; data reads and writes; read only reads"""


class Store:
    """The store in a directory, as this process reaches it. Close it once: ``async with`` does."""

    def __init__(self, link: Link) -> None:
        self._link = link
        self._workers: set[Worker] = set()
        self.clock = Clock(link)
        """The test's clock of a private store opened with ``clock``; on any other, its calls are InvalidError."""

    @overload
    def bucket[T](
        self,
        name: str,
        of: type[T],
        *,
        ttl: Duration | None = None,
        idle: Duration | None = None,
        encrypted: bool = False,
    ) -> Bucket[T]: ...
    @overload
    def bucket(
        self,
        name: str,
        of: None = None,
        *,
        ttl: Duration | None = None,
        idle: Duration | None = None,
        encrypted: bool = False,
    ) -> Bucket[Any]: ...
    def bucket(
        self,
        name: str,
        of: Any = None,
        *,
        ttl: Duration | None = None,
        idle: Duration | None = None,
        encrypted: bool = False,
    ) -> Bucket[Any]:
        """A bucket of values of one type by key; without a type, any JSON value, or keys alone as a set::

        sessions = store.bucket("sessions", Session, idle="30d")
        codes = store.bucket("login-codes", int, ttl="15m")
        seen = store.bucket("stripe-events", ttl="7d")

        A str, bytes, int, float or bool is kept as itself; a dataclass, a
        TypedDict, a pydantic model or a list or dict of them as JSON.
        ``encrypted=True`` keeps every value sealed with the store's
        encryption key.
        """
        opener = bucket_open(name, ttl, idle, encrypted, None)
        return Bucket(Parts(self._link, name, opener, "kv.bucket.open", (), values_of(of), None))

    def counters(self, name: str, *, ttl: Duration | None = None, flush_every: Duration | None = None) -> Counters:
        """Numbers by key that only add up: ``store.counters("login-attempts", ttl="15m")``.

        ``flush_every`` keeps counts in memory and writes them that often, so
        that an add costs no commit and a crash loses at most that span.
        """
        opener = counters_open(name, ttl, flush_every)
        return Counters(Parts(self._link, name, opener, "kv.counters.open", (), None, None))

    def rate_limit(self, name: str, *, rate: str, burst: int | None = None) -> RateLimit:
        """A smooth rate of requests a key: ``store.rate_limit("api", rate="100/s", burst=20)``."""
        opener = rate_limit_open(name, rate, burst)
        return RateLimit(Parts(self._link, name, opener, "kv.rateLimit.open", (), None, None))

    def quota(self, name: str, **windows: str) -> Quota:
        """Uses by key in named windows, each counted from a key's first use, all or none.

        ``store.quota("ai", daily="100/1d", weekly="300/7d")``
        """
        opener = quota_open(name, windows)
        return Quota(Parts(self._link, name, opener, "kv.quota.open", (), None, None))

    @overload
    def once[T](self, name: str, of: type[T], *, keep: Duration | None = None) -> Once[T]: ...
    @overload
    def once(self, name: str, of: None = None, *, keep: Duration | None = None) -> Once[Any]: ...
    def once(self, name: str, of: Any = None, *, keep: Duration | None = None) -> Once[Any]:
        """A function run once a key, its JSON answer kept a day unless keep says.

        ``store.once("charges", Receipt)``
        """
        opener = once_open(name, keep)
        return Once(Parts(self._link, name, opener, "kv.once.open", (), json_values(of), None))

    @overload
    def queue[T](
        self,
        name: str,
        of: type[T],
        *,
        attempts: int | None = None,
        backoff: Backoff | None = None,
        timeout: Duration | None = None,
        concurrency: int | Concurrency | None = None,
        rate: str | None = None,
        dedupe: Duration | None = None,
        keep: Duration | None = None,
        max_waiting: int | None = None,
    ) -> Queue[T]: ...
    @overload
    def queue(
        self,
        name: str,
        of: None = None,
        *,
        attempts: int | None = None,
        backoff: Backoff | None = None,
        timeout: Duration | None = None,
        concurrency: int | Concurrency | None = None,
        rate: str | None = None,
        dedupe: Duration | None = None,
        keep: Duration | None = None,
        max_waiting: int | None = None,
    ) -> Queue[Any]: ...
    def queue(
        self,
        name: str,
        of: Any = None,
        *,
        attempts: int | None = None,
        backoff: Backoff | None = None,
        timeout: Duration | None = None,
        concurrency: int | Concurrency | None = None,
        rate: str | None = None,
        dedupe: Duration | None = None,
        keep: Duration | None = None,
        max_waiting: int | None = None,
    ) -> Queue[Any]:
        """A queue of jobs of one type, JSON, run in the order of their time::

        emails = store.queue("emails", Email, attempts=20, backoff=Backoff(initial="5s", max="30m"))
        refreshes = store.queue("refreshes", Refresh, concurrency=Concurrency(total=8, group=2))
        """
        options = QueueOptions(attempts, backoff, timeout, concurrency, rate, dedupe, keep, max_waiting)
        return open_queue(self._link, self._workers, name, of, options)

    def schedule(
        self,
        name: str,
        handler: Callable[[Run], Awaitable[Outcome | None]],
        *,
        every: Duration | None = None,
        cron: str | None = None,
        time_zone: str | None = None,
        attempts: int | None = None,
        backoff: Backoff | None = None,
        timeout: Duration | None = None,
    ) -> Schedule:
        """A repeat the code owns: one repeating job under name, and handler run at each of its times.

        It starts at once, as a queue's work does. The code's repeat replaces
        the one kept each time the program opens it::

            cleanup = store.schedule("cleanup", cleanup_deleted, cron="10 3 * * *", time_zone="Europe/Berlin")
        """
        return open_schedule(
            self._link,
            self._workers,
            name,
            handler,
            every=every,
            cron=cron,
            time_zone=time_zone,
            attempts=attempts,
            backoff=backoff,
            timeout=timeout,
        )

    async def database(
        self, name: str, *, migrations: Migrations | None = None, durability: Durability | None = None
    ) -> Database:
        """The application's database name, sql/<name>.db, once its migrations are applied and checked.

        A migration that fails, or one changed after it was applied, is told
        here, at the start. Without migrations the file opens as it is, an
        empty one when there is none::

            db = await store.database("app", migrations=Path(__file__).parent / "migrations")
        """
        return await open_database(self._link, self._workers, name, migrations, durability)

    async def tx[R](self, fn: Callable[[Tx], Awaitable[R]]) -> R:
        """Runs fn as one transaction over the handles it takes in with ``tx.with_(handle)``, and gives what it returns.

        Reads go at once; writes commit together when fn returns, after a check
        that nothing it read has changed, and fn runs again when something did,
        five times at most, so it does nothing else that must happen once.
        """
        return await run_tx(self._link, fn)

    async def status(self) -> Status:
        """What the server this store reaches is: its version, protocol, engines and capability.

        A client newer than its server learns here what it may ask for; a call
        past it is UnimplementedError, naming the server's version.
        """
        agreed = await (await self._link.connection()).session.welcomed
        capability: Literal["admin", "data", "read"] = (
            "admin" if agreed.capability == "admin" else "read" if agreed.capability == "read" else "data"
        )
        return Status(agreed.server, agreed.protocol, list(agreed.engines), capability)

    async def close(self) -> None:
        """Stops every worker, each once its handlers under way have answered, then closes the connection.

        A private child or the core in this process lets the directory go
        before this returns; the directory's sidecar goes once it has been idle.
        """
        await asyncio.gather(*(worker.stop() for worker in list(self._workers)), return_exceptions=True)
        await self._link.close()

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(
        self, kind: type[BaseException] | None, error: BaseException | None, trace: TracebackType | None
    ) -> None:
        await self.close()


async def open(
    directory: str | os.PathLike[str],
    *,
    private: bool = False,
    embedded: bool = False,
    library: str | None = None,
    binary: str | None = None,
    idle: Duration | None = None,
    clock: datetime | None = None,
    durability: Durability | None = None,
    encryption_key_file: str | os.PathLike[str] | None = None,
) -> Store:
    """Opens the store in a directory, and returns once it answers.

    Through the directory's sidecar, found through its SERVE file or started,
    unless asked otherwise: ``private=True`` starts a server of this process's
    own, a child that lives and dies with it, for tests and scripts;
    ``embedded=True`` runs the core inside this process, through the module
    this platform's wheel carries or the ``library`` given. A directory that
    cannot be served fails here.

    - ``binary``: the tinystore binary; TINYSTORE_BIN, this platform's wheel's
      or PATH's when absent.
    - ``idle``: how long a sidecar this process starts stays once its last
      connection has gone; 30 s unless given, ``"0s"`` for ever. One found
      running keeps its own.
    - ``clock``: runs a private server on a test's clock, starting at this
      time, which ``store.clock`` moves forward instead of a test waiting.
    - ``durability``: how far a commit of the store's files goes before it
      returns, ``"full"`` unless given; ``"os"`` does not wait for the disk.
      It is the store's, set by whoever opens it first: one found open with
      another is refused.
    - ``encryption_key_file``: the file of the store's encryption key, 64 hex
      digits, kept apart from the data; the store reads it and never makes it.
    """
    where = _directory(directory)
    check_durability(durability, "open")
    if clock is not None and not private:
        raise InvalidError("a clock is a private server's: a shared sidecar runs on the system's time")
    if private and embedded:
        raise InvalidError("a store is private or embedded, not both")
    key = None if encryption_key_file is None else str(encryption_key_file)
    started = Started(
        durability=durability,
        encryption_key_file=key,
        idle=None if idle is None else spelled(ms(idle, "a sidecar's idle")),
    )

    def find() -> str:
        return _runtime.find_binary(binary)

    if embedded:
        dial = in_process(where, library, started)
    elif private:
        dial = private_child(where, find, clock, started)
    else:
        dial = sidecar(where, find, started)
    link = Link(asking(dial, where, durability))
    await link.connection()
    return Store(link)


async def connect(url: str, *, token: str, tls: ssl.SSLContext | None = None) -> Store:
    """Connects to a remote server.

    ``tls://`` checks the server's certificate before the token leaves;
    ``tcp://`` sends the token in the clear, for a network its operator trusts.
    """
    link = Link(remote(url, token, tls))
    await link.connection()
    return Store(link)


# two letters or more before a colon: an address, never a Windows drive
_ADDRESS = re.compile(r"([a-z][a-z0-9+.-]+):", re.IGNORECASE)


def _directory(directory: str | os.PathLike[str]) -> str:
    """Refuses an address where a directory belongs.

    ``open("tcp://...")`` would make a folder of that name and start a server
    in it, rather than reach the one meant.
    """
    text = str(directory)
    scheme = _ADDRESS.match(text)
    if scheme is not None and scheme[1].lower() in ("tcp", "tls"):
        raise InvalidError(
            f"open takes a store's directory, and {text} is a server's address: connect({text!r}, token=...) reaches it"
        )
    if scheme is not None:
        raise InvalidError(
            f"open takes a store's directory, and {text} is an address: open the directory its server serves"
        )
    return text
