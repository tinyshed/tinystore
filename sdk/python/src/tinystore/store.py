"""The store in a directory, and how a program reaches it.

::

    async with await tinystore.open("data") as store:
        sessions = store.bucket("sessions", Session, idle="30d")
        await sessions.under(user.id).set(digest(token), session)
"""

from __future__ import annotations

import asyncio
import logging
import re
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Literal, Self, overload

from . import _runtime
from ._background import message_of, say_own
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
from .backup import Backup, backup
from .blobs import Files, Size, open_files
from .channels import Channel
from .clock import Clock
from .config import Config
from .errors import InvalidError
from .instruments import Instruments
from .jobs import Backoff, Concurrency, Outcome, Queue, QueueOptions, Run, Schedule, Worker, open_queue, open_schedule
from .kv import Bucket, Counters, Parts, bucket_open, counters_open
from .limits import Quota, RateLimit, quota_open, rate_limit_open
from .locks import Lock, Semaphore
from .logger import Level, Logger, LoggerOptions, LogHandler, new_store_logger
from .metrics import Metrics
from .once import Once, once_open
from .records import AllEvents, Events, open_events
from .search import Fields, Hybrid, IndexInfo, SearchIndex, Vectors, drop_index, index_open, list_indexes
from .sql import Database, Migrations, open_database
from .tx import Tx, run_tx

if TYPE_CHECKING:
    import os
    import ssl
    from collections.abc import Awaitable, Callable, Iterable, Sequence
    from datetime import datetime
    from types import TracebackType
    from typing import TextIO

    from ._console import Console, ConsoleTime
    from .config import Layer


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
        self._following: set[asyncio.Future[None]] = set()
        self._loggers: list[Logger] = []
        self._configs: dict[tuple[str, type | None, str, bool], tuple[asyncio.Future[Config[Any]], object]] = {}
        """the configs open, by what opened them, so that the same open shares one stream"""
        self._instruments = Instruments(link)
        self.metrics = Metrics(link, self._instruments)
        """Series by name and labels, their reads and aggregates, and the program's instruments."""
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

    @overload
    def channel[T](self, name: str, of: type[T]) -> Channel[T]: ...
    @overload
    def channel(self, name: str, of: None = None) -> Channel[Any]: ...
    def channel(self, name: str, of: Any = None) -> Channel[Any]:
        """A channel of messages of one type, JSON, which every subscriber listening at that moment receives::

        messages = store.channel("messages", Chat)
        await messages.under(room_id).publish(Chat(sender="ann", text="hi"))
        sub = await messages.under(room_id).subscribe(lambda m: show(m.value))
        """
        return Channel(self._link, name, of)

    def lock(self, name: str, *, ttl: Duration = "10s", max_hold: Duration | None = None) -> Lock:
        """Keys held one holder at a time, across every program of the store::

            invoices = store.lock("invoices", ttl="10s")
            async with invoices.hold(invoice.id) as lease:
                await charge(invoice, fence=lease.fence)

        - ``ttl``: how long a lease lasts from its last renewal, 1 s to 1 hour;
          this program renews it at a third of it while the block runs.
        - ``max_hold``: how long a lease lasts from its grant, however it is
          renewed: past it the next in line holds the key.
        """
        return Lock(self._link, name, ttl, max_hold)

    def semaphore(self, name: str, *, limit: int, ttl: Duration = "10s", max_hold: Duration | None = None) -> Semaphore:
        """Keys held by up to limit holders at a time, 1 to 1000::

        uploads = store.semaphore("uploads", limit=3, ttl="30s")
        async with uploads.hold(user.id):
            await upload(file)
        """
        return Semaphore(self._link, name, limit, ttl, max_hold)

    def files(self, name: str, *, ttl: Duration | None = None, max_file_size: Size | None = None) -> Files:
        """A named set of files by path, written whole and replaced whole::

        avatars = store.files("avatars")
        reports = store.files("reports", ttl="24h", max_file_size="2GiB")
        """
        return open_files(self._link, name, ttl, max_file_size)

    @overload
    def events(self) -> AllEvents: ...
    @overload
    def events(self, stream: str, *, keep: Duration | None = None) -> Events: ...
    def events(self, stream: str | None = None, *, keep: Duration | None = None) -> Events | AllEvents:
        """A stream of events by name, made with its first event; without a name, every stream, to read::

        audit = store.events("audit", keep="365d")
        await audit.append("role.changed", {"user": 42, "role": "admin"})
        page = await store.events().list(since="1h", min_level="warn")
        """
        if stream is None:
            if keep is not None:
                raise InvalidError("keep is a stream's: name the stream")
            return AllEvents(self._link)
        return open_events(self._link, stream, keep)

    def logger(
        self,
        stream: str,
        *,
        level: Level | None = None,
        console: Console | None = None,
        time: ConsoleTime | None = None,
        hide_stream: bool = False,
        to: TextIO | None = None,
        redact: Iterable[str] = (),
        keep_url_passwords: bool = False,
        replace: Callable[[str, Any], Any] | None = None,
        source: bool = False,
        env: str | bool = True,
        buffer: int = 1024,
        keep: Duration | None = None,
    ) -> Logger:
        """A logger of the stream: each line printed as it is logged and kept within a second.

        A call never waits; what its buffer has no room for is dropped and
        counted. The store's close hands over what it holds::

            log = store.logger("api", redact=tinystore.secrets)
            log.info("signed in", user=42)
        """
        options = LoggerOptions(
            level, console, time, hide_stream, to, redact, keep_url_passwords, replace, source, env, buffer
        )
        events = open_events(self._link, stream, keep)
        made, log = new_store_logger(events.append_lines, stream, options)
        self._loggers.append(made)
        follows = asyncio.ensure_future(self._follow_log(log.layers, log.take))
        self._following.add(follows)
        follows.add_done_callback(self._following.discard)
        return made

    async def _follow_log(self, layers: list[Any], take: Callable[[str], None]) -> None:
        """Sets a logger's level from the config log, which a change of any process sets for every logger."""
        global _unfollowed
        try:
            log = await self.config("log", layers)
        except Exception as err:
            if not self._link.closed and not _unfollowed:
                _unfollowed = True
                say_own("warn", "a logger follows no config log", {"error": message_of(err)})
            return
        log.on_change(lambda value: take(value.level))

    def log_handler(self, stream: str, level: int = logging.NOTSET, **options: Any) -> LogHandler:
        """A handler for a program that logs through logging, its lines printed and kept as the store's logger's::

        logging.getLogger("worker").addHandler(store.log_handler("worker"))

        A record's extra are its fields, its logger's name in its context.
        The options are the store's logger's.
        """
        return LogHandler(self.logger(stream, **options), level)

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

    @overload
    def index[T](
        self,
        name: str,
        of: type[T],
        *,
        fields: Fields | None = None,
        filters: Sequence[str] = (),
        typo_tolerance: bool | None = None,
        vectors: Vectors | None = None,
        hybrid: Hybrid | None = None,
        rebuild: Literal["automatic", "manual"] | None = None,
    ) -> SearchIndex[T]: ...
    @overload
    def index(
        self,
        name: str,
        of: None = None,
        *,
        fields: Fields | None = None,
        filters: Sequence[str] = (),
        typo_tolerance: bool | None = None,
        vectors: Vectors | None = None,
        hybrid: Hybrid | None = None,
        rebuild: Literal["automatic", "manual"] | None = None,
    ) -> SearchIndex[Any]: ...
    def index(
        self,
        name: str,
        of: Any = None,
        *,
        fields: Fields | None = None,
        filters: Sequence[str] = (),
        typo_tolerance: bool | None = None,
        vectors: Vectors | None = None,
        hybrid: Hybrid | None = None,
        rebuild: Literal["automatic", "manual"] | None = None,
    ) -> SearchIndex[Any]:
        """An index of documents of one type by id, found again by their words, vectors or both::

            articles = store.index("articles", Article, fields={"title": 2, "body": 1})
            result = await articles.search("как испечь хлеб")

        Opening an index with other settings than it holds changes it, and
        one that needs every document read again does so beside it, the old
        rows answering until the new ones switch in; ``rebuild="manual"``
        waits for ``store.reindex``.
        """
        opener = index_open(
            name,
            fields=fields,
            filters=filters,
            typo_tolerance=typo_tolerance,
            vectors=vectors,
            hybrid=hybrid,
            rebuild=rebuild,
        )
        return SearchIndex(self._link, name, opener, of)

    async def reindex(
        self,
        name: str,
        of: Any = None,
        *,
        fields: Fields | None = None,
        filters: Sequence[str] = (),
        typo_tolerance: bool | None = None,
        vectors: Vectors | None = None,
        hybrid: Hybrid | None = None,
        rebuild: Literal["automatic", "manual"] | None = None,
    ) -> SearchIndex[Any]:
        """Opens an index as index does and reads it again beside itself in the background, as a change would."""
        opener = index_open(
            name,
            fields=fields,
            filters=filters,
            typo_tolerance=typo_tolerance,
            vectors=vectors,
            hybrid=hybrid,
            rebuild=rebuild,
            reindex=True,
        )
        index: SearchIndex[Any] = SearchIndex(self._link, name, opener, of)
        await index.status()
        return index

    async def drop_index(self, name: str) -> bool:
        """Drops an index and every document it holds, in the background; says whether there was one."""
        return await drop_index(self._link, name)

    async def indexes(self) -> list[IndexInfo]:
        """Every index the store holds: its documents and vectors, when it was last opened, and a rebuild under way."""
        return await list_indexes(self._link)

    async def tx[R](self, fn: Callable[[Tx], Awaitable[R]]) -> R:
        """Runs fn as one transaction over the handles it takes in with ``tx.with_(handle)``, and gives what it returns.

        Reads go at once; writes commit together when fn returns, after a check
        that nothing it read has changed, and fn runs again when something did,
        five times at most, so it does nothing else that must happen once.
        """
        return await run_tx(self._link, fn)

    @overload
    async def config[T](
        self,
        name: str,
        layers: Sequence[type[T] | Layer],
        *,
        validate: Callable[[T], object] | type | None = None,
        strict: bool = False,
    ) -> Config[T]: ...
    @overload
    async def config(
        self,
        name: str,
        layers: Sequence[Any],
        *,
        validate: Callable[[Any], object] | type | None = None,
        strict: bool = False,
    ) -> Config[Any]: ...
    async def config(
        self,
        name: str,
        layers: Sequence[Any],
        *,
        validate: Callable[[Any], object] | type | None = None,
        strict: bool = False,
    ) -> Config[Any]:
        """A program's settings as one typed value: the declaration first, then dicts, files and the environment.

        Each layer goes over the ones before it, and a live setting's change
        over them all::

            config = await store.config("app", [Settings, file("app.yaml"), env("APP", vars={"port": "PORT"})])

        It follows every change, whoever makes it, until the store closes; the
        same config opened again in the process shares its one stream.
        ``validate``, a function that raises or a pydantic model, checks the
        whole value at open and before a change is kept; ``strict`` refuses to
        open while a kept change no longer fits, rather than ignoring it.
        """
        # the declaration by itself, since two classes of one name may declare apart
        key = (name, layers[0] if layers and isinstance(layers[0], type) else None, repr(list(layers[1:])), strict)
        found = self._configs.get(key)
        if found is not None and found[1] is validate:
            return await asyncio.shield(found[0])
        opening = asyncio.ensure_future(Config[Any].open(self._link, name, layers, validate, strict))
        self._configs[key] = (opening, validate)
        try:
            return await asyncio.shield(opening)
        except BaseException:
            if key in self._configs and self._configs[key][0] is opening:
                del self._configs[key]
            raise

    async def backup(
        self,
        path: str | os.PathLike[str],
        *,
        include: Sequence[str] = (),
        include_encryption_key: bool = False,
    ) -> Backup:
        """A backup of every engine of the store as one zip, made while the store keeps working.

        It is written beside path and renamed once whole, so that a backup
        that fails leaves no zip. A file of the program's goes in only when
        ``include`` names it by its path in the store's directory, and the
        store's encryption key only with ``include_encryption_key``, the zip
        not being encrypted::

            backup = await store.backup("backup-2026-10-13.zip", include=["secret.key"])
            if backup.key_left_out:
                print("keep encryption.key apart: the backup opens with it alone")
        """
        return await backup(self._link, path, include, include_encryption_key)

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
        await self._instruments.stop()
        await asyncio.gather(*(each.stop() for each in self._loggers), return_exceptions=True)
        opened = [
            opening.result() for opening, _ in self._configs.values() if opening.done() and not opening.exception()
        ]
        await asyncio.gather(*(config.stop() for config in opened), return_exceptions=True)
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
    self_metrics: bool = False,
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
    - ``self_metrics``: measures the store into its own metrics, under names
      that start with ``tinystore_``: its commits and their waits, its files,
      its memory. Off unless asked for; a sidecar found running keeps its own.
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
        self_metrics=self_metrics,
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


_unfollowed = False
"""Whether a logger that follows no config log has been said, once a process."""

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
