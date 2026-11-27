"""A function run once a key, its answer kept.

A request sent twice is answered as the first was rather than done twice. The
core lets one run of a key go at a time, every client of the store included:
it answers the kept answer, or hands the run to this client and keeps what it
sends back::

    charges = store.once("charges", Receipt, keep="1d")
    receipt = await charges.run(request_id, lambda: pay.charge(order, request_id))

Opened with ``fresh``, a kept answer older than it is answered at once, and the
core may hand this client the build of the next one, which runs in the
background::

    books = store.once("cabinet-books", GradeBook, fresh="10m", keep="12h")
    answer = await books.fetch(str(user_id), lambda: cabinet.book(user_id))
"""

from __future__ import annotations

import asyncio
import contextlib
import contextvars
import inspect
from dataclasses import dataclass
from datetime import timedelta
from typing import TYPE_CHECKING, Any, cast

from ._connection import check_name
from ._handles import handle_on, via_link
from ._session import ConnectionLostError, Stream
from ._time import Duration, ms
from ._wire.protocol import KvAnswer, KvBranch, KvCall, KvFound, KvOnceOpen, methods
from .errors import OutcomeUnknownError, ProtocolError
from .kv import Parts, below, call_on, under_field
from .locks import outside_holds

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

    from ._connection import Connection
    from ._values import Values
    from ._wire.codec import Key


_BUILDING: set[asyncio.Task[None]] = set()
"""The builds in the background under way, held until they end."""


@dataclass(frozen=True, slots=True)
class OnceOpens:
    """What a once's opens send: ``aged`` asks for each answer's age, which
    ``fetch`` reads and ``run`` does not, so that ``run`` without ``fresh``
    opens as an older client did and still reaches an older server."""

    plain: bytes
    aged: bytes


def once_open(name: str, keep: Duration | None, fresh: Duration | None = None) -> OnceOpens:
    """Only this client, which reads ``refresh``, sends ``fresh``: the core hands
    a build in the background only to a handle opened with it, so an older client
    never reads one as a run of its own. A handle with ``fresh`` opens aged."""
    check_name(name, "once")
    kept = None if keep is None else ms(keep, f"kv once {name}: keep")
    fresh_ms = None if fresh is None else ms(fresh, f"kv once {name}: fresh")
    aged = KvOnceOpen.encode(name=name, keep=kept, fresh=fresh_ms, aged=True)
    plain = KvOnceOpen.encode(name=name, keep=kept) if fresh_ms is None else aged
    return OnceOpens(plain, aged)


@dataclass(frozen=True, slots=True)
class Fetched[T]:
    """An answer with how old it is: stale once older than the ``fresh`` its answers opened with."""

    value: T
    age: timedelta
    stale: bool


class Once[T]:
    """Answers kept once a key: open them with ``store.once(name, type)``."""

    def __init__(self, parts: Parts, aged: bytes | None = None) -> None:
        self.name = parts.name
        self._parts = parts
        self._aged = aged if aged is not None else cast("bytes", parts.opener)
        assert parts.values is not None
        self._values: Values[T] = parts.values

    def under(self, *owners: Key) -> Once[T]:
        """The answers of a branch, each key apart from the same key elsewhere."""
        return Once(below(self._parts, owners), self._aged)

    async def run(self, key: Key, fn: Callable[[], Awaitable[T] | T]) -> T:
        """The answer kept for key, or fn's, which is kept.

        A run of a key that another runs, here or in another process, waits for
        it and gets its answer; an exception of fn keeps nothing and is raised,
        so the next run runs fn again. A connection lost after fn returned and
        before its answer was kept is OutcomeUnknownError.
        """
        return (await self._ask(key, fn, cast("bytes", self._parts.opener))).value

    async def fetch(self, key: Key, fn: Callable[[], Awaitable[T] | T]) -> Fetched[T]:
        """run's answer with how old it is.

        Opened with ``fresh``, a kept answer older than it comes back at once,
        stale, and fn may make the next one in the background: its answer
        replaces the kept one, and its exception, which no caller sees, keeps it
        until ``keep`` ends.
        """
        return await self._ask(key, fn, self._aged)

    async def _ask(self, key: Key, fn: Callable[[], Awaitable[T] | T], opener: bytes) -> Fetched[T]:
        handed = await self._claim(key, opener)
        if isinstance(handed, Fetched):
            return handed
        stream, stale = handed
        if stale is not None:
            self._background(stream, fn)
            return stale
        try:
            answer = await _called(fn)
            kept = KvAnswer.encode(found=True, value=self._values.encode(answer))
        except BaseException:
            # fn's own error is the one to raise
            with contextlib.suppress(Exception):
                await _end(stream, KvAnswer.encode())
            raise
        await _end(stream, kept)
        return Fetched(answer, timedelta(0), False)

    def _background(self, stream: Stream, fn: Callable[[], Awaitable[T] | T]) -> None:
        """Runs a build the core handed over in the background, its caller answered
        already. A connection lost or a store closed meanwhile ends it with nothing
        kept, and nothing is raised, since no caller waits for it."""

        async def build() -> None:
            try:
                kept = KvAnswer.encode(found=True, value=self._values.encode(await _called(fn)))
            except Exception:
                kept = KvAnswer.encode()
            with contextlib.suppress(Exception):
                await _end(stream, kept)

        # the keys the caller holds end with its call, which the build outlives
        context = contextvars.copy_context()
        context.run(outside_holds)
        task = asyncio.get_running_loop().create_task(build(), context=context)
        # The store's close cancels the stream it follows, and with it the
        # build, which nobody else awaits; a task the loop alone holds may be
        # collected before its end.
        let_go = self._parts.link.follow(stream)
        finishing = asyncio.ensure_future(stream.finishing())
        finishing.add_done_callback(lambda _: task.cancel())
        _BUILDING.add(task)

        def ended(_: asyncio.Task[None]) -> None:
            _BUILDING.discard(task)
            let_go()
            finishing.cancel()

        task.add_done_callback(ended)

    async def get(self, key: Key) -> T | None:
        """The answer kept for key, None when none is."""
        kept = KvAnswer.decode(await call_on(self._parts, "kv.once.get", {"key": key}, "read"))
        return self._values.decode(kept.get("value")) if kept.get("found") else None

    async def delete(self, key: Key) -> bool:
        """Forgets the answer kept for key, so that its next run runs fn again."""
        return KvFound.decode(await call_on(self._parts, "kv.once.delete", {"key": key}, "write")).get("found", False)

    async def clear(self) -> None:
        """Forgets every answer of this branch and of the branches under it, in one commit.

        A run under way meanwhile keeps its answer once it ends.
        """
        under = under_field(self._parts.under)
        await via_link(self._parts.link).call(
            methods["kv.once.open"],
            self._parts.opener,
            methods["kv.clear"],
            lambda handle: KvBranch.encode(handle=handle, under=under),
            "write",
        )

    async def _claim(self, key: Key, opener: bytes) -> tuple[Stream, Fetched[T] | None] | Fetched[T]:
        # asks for key's run: the answer kept for it, or the run the core handed
        # over, which a connection lost before the answer asks for again
        async def attempt(connection: Connection) -> tuple[Stream, Fetched[T] | None] | Fetched[T]:
            handle = await handle_on(connection, methods["kv.once.open"], opener)
            call = KvCall.encode(handle=handle, under=under_field(self._parts.under), key=key)
            stream = await connection.session.open(methods["kv.once.run"], call, False)
            try:
                first = await stream.next()
                offered: dict[str, Any] = KvAnswer.decode(first.body)
                if not first.end:
                    return (stream, self._kept(offered) if offered.get("refresh") else None)
                if not offered.get("found"):
                    raise ProtocolError(f"kv once {self.name}: a run ended without its answer")
                return self._kept(offered)
            except BaseException:
                stream.cancel()
                raise

        return await self._parts.link.run("read", attempt)

    def _kept(self, offered: dict[str, Any]) -> Fetched[T]:
        age = timedelta(milliseconds=offered.get("age", 0))
        return Fetched(self._values.decode(offered.get("value")), age, bool(offered.get("stale")))


async def _called[T](fn: Callable[[], Awaitable[T] | T]) -> T:
    answered = fn()
    return cast("T", await answered) if inspect.isawaitable(answered) else answered


async def _end(stream: Stream, answer: bytes) -> None:
    """Sends the run's answer as the stream's last DATA, and waits for the core's, once it is kept."""
    try:
        await stream.send(answer, True)
        await stream.next()
    except ConnectionLostError as err:
        raise OutcomeUnknownError(
            f"the connection was lost before the answer was kept; the next run may run again: {err}"
        ) from err
