"""Locks and semaphores, as docs/guides/locks/README.md has them.

A key that one holder at a time, or ``limit``, may hold while a block runs,
across every program of a store. The store keeps them in its process's
memory; a holder here renews its lease at a third of its ttl from this
program, so that a program that froze stops renewing and loses the key::

    invoices = store.lock("invoices", ttl="10s")
    async with invoices.hold(invoice.id) as lease:
        await charge(invoice, fence=lease.fence)
"""

from __future__ import annotations

import asyncio
import contextlib
import contextvars
import json
import secrets
import time
import weakref
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from typing import TYPE_CHECKING, Literal

from ._connection import check_name
from ._handles import key_text
from ._session import ConnectionLostError
from ._time import date_of, ms, spelled
from ._wire.protocol import LocksHeld, LocksHold, LocksLease, LocksRenew, LocksState, LocksStateAsked, methods
from .errors import ClosedError, InUseError, InvalidError, LostError
from .sql import inside_database_tx
from .tx import inside_store_tx

if TYPE_CHECKING:
    from collections.abc import Callable
    from types import TracebackType
    from typing import Any

    from ._connection import Link
    from ._time import Duration
    from ._wire.codec import Key

_WAIT = "10s"

type Priority = Literal["urgent", "background"]
"""Which line a semaphore's hold waits in.

Urgent holds are given a place before background ones, and a background hold
waiting never keeps an urgent one from a free place. A background hold is
promised no place: it waits while urgent holds keep coming, ``wait`` at most.
"""


@dataclass(frozen=True, slots=True)
class _Asked:
    """How a hold asks for its key: how long it waits, in which line, and how long it may keep it."""

    wait: int
    priority: Priority = "urgent"
    timeout: int | None = None


_holding: contextvars.ContextVar[tuple[tuple[int, str], ...]] = contextvars.ContextVar("tinystore holding", default=())
"""The keys the caller holds or waits for, each with the link of the store it is of."""


def outside_holds() -> None:
    """Holds no key from here on in this context: work that outlives the hold its caller is inside."""
    _holding.set(())


_opened: weakref.WeakKeyDictionary[Link, dict[str, _Shape]] = weakref.WeakKeyDictionary()
"""The limit and the background each semaphore's first open gave, by the link of the store that opened it.

One name keeps one limit in a program, and two stores are two programs, each
of whose holds the store admits by its own limit.
"""


type _Shape = tuple[int, int | None, tuple[int, int] | None]
"""What a semaphore's first open gives: its limit, its background, and its breaker's failures and pause."""


def _breaking(name: str, breaker: Breaker | None) -> tuple[int, int] | None:
    """A breaker's failures and its pause in milliseconds, checked."""
    if breaker is None:
        return None
    pause = ms(breaker.pause, f"semaphore {name}: its breaker's pause")
    failures = breaker.failures
    if isinstance(failures, bool) or not isinstance(failures, int) or failures < 1 or pause == 0:  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(
            f"semaphore {name}: a breaker of {failures!r} failures and {pause} ms, not 1 or more and above 0"
        )
    return (failures, pause)


def _keep_shape(link: Link, name: str, given: _Shape) -> None:
    """Keeps the limit, background and breaker a semaphore's first open on this store gives: another is refused."""
    first = _opened.setdefault(link, {}).setdefault(name, given)

    def refused(what: str, before: object, now: object) -> InvalidError:
        return InvalidError(f"semaphore {name}: open in this program with a {what} of {before}, not {now}")

    if first[0] != given[0]:
        raise refused("limit", first[0], given[0])
    if first[1] != given[1]:
        raise refused("background", "none" if first[1] is None else first[1], "none" if given[1] is None else given[1])
    if first[2] != given[2]:
        shown = (
            "none" if rule is None else f"{rule[0]} failures and {spelled(rule[1])}" for rule in (first[2], given[2])
        )
        raise refused("breaker", *shown)


@dataclass(frozen=True, slots=True)
class Lease:
    """A holding of a lock's key."""

    fence: int
    """grows with every lease the store gives, 128 bits: hand it to whatever must refuse a late holder"""
    waited: timedelta
    """how long the hold waited for the key, from its call to the grant, the round trip included"""


@dataclass(frozen=True, slots=True)
class Permit:
    """A holding of a semaphore's key: one of its limit."""

    waited: timedelta
    """how long the hold waited for its place, from its call to the grant, the round trip included"""
    _healthy: Callable[[], None] = field(repr=False, compare=False, default=lambda: None)

    def healthy(self) -> None:
        """Says the guarded service is healthy, so that the breaker counts no failure though the block then raises."""
        self._healthy()


@dataclass(frozen=True, slots=True)
class Breaker:
    """A semaphore's breaker: ``failures`` failed holds of a key in a row put it down for ``pause``.

    A hold failed when its block raised, unless its permit said
    ``healthy()``, or when it ran past its timeout or max_hold; a success
    resets the count. While the key is down every hold is UnavailableError at
    once. Once the pause is over, one hold goes through: its success puts the
    key up, its failure pauses it again. The store's process keeps it in
    memory, so a restart of that process puts every key up.
    """

    failures: int
    pause: Duration


@dataclass(frozen=True, slots=True)
class KeyState:
    """How a semaphore's key stands now: ``await semaphore.state(key)``."""

    up: bool
    """false from the failure that put the key down until a hold after the pause succeeds; true without a breaker"""
    down_since: datetime | None
    last_failure: datetime | None
    """the last failed hold's end, kept until a success after it"""
    waiting: int
    holding: int
    failures: int
    """the failed holds in a row the breaker counts now, toward its ``failures``:
    reset by a success, and by a pause with no failure; while the key is down,
    those that put it down; 0 without a breaker"""


class _Keys:
    """What a lock and a semaphore share: asking for a key, and the lease once given."""

    def __init__(
        self,
        link: Link,
        kind: Literal["lock", "semaphore"],
        name: str,
        limit: int,
        ttl: Duration,
        max_hold: Duration | None,
        background: int | None = None,
        breaker: tuple[int, int] | None = None,
    ) -> None:
        check_name(name, kind)
        self._link = link
        self._kind = kind
        self._name = name
        self._limit = limit
        self._background = background
        self._breaker = breaker
        self._ttl = ms(ttl, f"{kind} {name}: its ttl")
        self._max_hold = None if max_hold is None else ms(max_hold, f"{kind} {name}: its max_hold")

    def place(self, key: Key) -> str:
        """The key as an error names it: lock invoices: key "42"."""
        return f"{self._kind} {self._name}: key {json.dumps(key_text(key))}"

    def refuse(self, place: str) -> None:
        """Refuses a hold that would wait for what its caller holds: a writer a transaction keeps, or the same key."""
        if inside_store_tx() or inside_database_tx():
            raise InvalidError(
                f"{place}: a hold inside a transaction would keep its writer while the key is waited for: hold first"
            )
        if (id(self._link), place) in _holding.get():
            raise InvalidError(f"{place}: a hold of a key inside its own hold would wait for itself")

    async def ask(self, key: Key, asked: _Asked) -> dict[str, Any]:
        """The key, once the store gives it, or that it was held for all of wait.

        A connection lost on the way asks again with the same request, which
        the store answers with the lease it gave, or with the ask's place in
        line. A hold's timeout is a max_hold of its own: the shorter revokes.
        """
        bounds = [bound for bound in (self._max_hold, asked.timeout) if bound is not None]
        body = LocksHold.encode(
            kind=self._kind,
            name=self._name,
            key=key_text(key),
            limit=self._limit,
            ttl=self._ttl,
            max_hold=min(bounds) if bounds else None,
            wait=asked.wait,
            request=secrets.token_bytes(16),
            priority="background" if asked.priority == "background" else None,
            background=self._background,
            failures=None if self._breaker is None else self._breaker[0],
            pause=None if self._breaker is None else self._breaker[1],
        )
        answer = await self._link.run("read", lambda c: c.session.call(methods["locks.hold"], body))
        return LocksHeld.decode(answer)

    async def state(self, key: Key) -> KeyState:
        body = LocksStateAsked.encode(kind=self._kind, name=self._name, key=key_text(key))
        state = LocksState.decode(await self._link.run("read", lambda c: c.session.call(methods["locks.state"], body)))
        return KeyState(
            up=bool(state.get("up")),
            down_since=date_of(state.get("down_since")),
            last_failure=date_of(state.get("last_failure")),
            waiting=state.get("waiting", 0),
            holding=state.get("holding", 0),
            failures=state.get("failures", 0),
        )


class Holding[V]:
    """A hold of a key for the block of an ``async with``, which gives what the block holds it by.

    A lease lost while the block runs cancels it at its next await, as
    ``asyncio.timeout`` does, and the block ends LostError; one lost after its
    last await ends it LostError too, since another holder may have run beside it.
    """

    def __init__(self, keys: _Keys, key: Key, asked: _Asked, view: type[V], tries: bool) -> None:
        self._keys = keys
        self._key = key
        self._asked = asked
        self._view = view
        self._tries = tries
        self._place = keys.place(key)
        self._lease = b""
        self._renewing: asyncio.Task[None] | None = None
        self._running_out: asyncio.TimerHandle | None = None
        self._revoking: asyncio.TimerHandle | None = None
        self._lost: LostError | ClosedError | None = None
        self._task: asyncio.Task[Any] | None = None
        self._cancelling = 0
        self._cancel_sent = False
        self._token: contextvars.Token[tuple[tuple[int, str], ...]] | None = None
        self._inside = False
        self._said_healthy = False

    async def __aenter__(self) -> V:
        # a holding entered again says nothing of its last hold's health
        self._said_healthy = False
        self._keys.refuse(self._place)
        asked = time.monotonic()
        answer = await self._keys.ask(self._key, self._asked)
        waited = timedelta(seconds=time.monotonic() - asked)
        if not answer.get("acquired"):
            if self._tries:
                return None  # type: ignore[return-value]
            raise InUseError(f"{self._place}: held for all of the wait")
        self._start(answer)
        self._token = _holding.set((*_holding.get(), (id(self._keys._link), self._place)))  # pyright: ignore[reportPrivateUsage]
        self._task = asyncio.current_task()
        self._inside = True
        fence = answer.get("fence") or "0"
        if self._view is Lease:
            return Lease(int(fence), waited)  # type: ignore[return-value]
        return Permit(waited, self._say_healthy)  # type: ignore[return-value]

    def _say_healthy(self) -> None:
        self._said_healthy = True

    async def __aexit__(
        self, kind: type[BaseException] | None, error: BaseException | None, trace: TracebackType | None
    ) -> bool | None:
        if not self._inside:
            return None
        self._inside = False
        if self._token is not None:
            _holding.reset(self._token)
        self._stop()
        lost = self._lost
        if lost is None:
            await self._release(failed=kind is not None and not self._said_healthy)
            return None
        # a lease lost lets nothing go, so nothing waits here while a cancel this sent may still be pending
        if self._cancel_sent and self._task is not None:
            others = self._task.uncancel() > self._cancelling
            if others and kind is not None and issubclass(kind, asyncio.CancelledError):
                return None
        raise lost from error

    def _start(self, answer: dict[str, Any]) -> None:
        loop = asyncio.get_running_loop()
        ttl = self._keys._ttl  # pyright: ignore[reportPrivateUsage]
        self._lease = answer.get("lease", b"")
        self._renewing = asyncio.ensure_future(self._renew_every(ttl))
        self._run_out_at(time.monotonic() + ttl / 1000)
        revoke_in = answer.get("revoke_in")
        if revoke_in is not None:
            self._revoking = loop.call_later(
                revoke_in / 1000, self._lose, "it was revoked at its max_hold or its timeout", None
            )

    async def _renew_every(self, ttl: int) -> None:
        while True:
            await asyncio.sleep(max(1, ttl // 3) / 1000)
            await self._renew(ttl)

    async def _renew(self, ttl: int) -> None:
        sent = time.monotonic()
        body = LocksRenew.encode(lease=self._lease, ttl=ttl)
        link = self._keys._link  # pyright: ignore[reportPrivateUsage]
        try:
            await link.run("read", lambda c: c.session.call(methods["locks.renew"], body))
            self._run_out_at(sent + ttl / 1000)
        except LostError:
            self._lose("it ran out before it was renewed", None)
        except ClosedError as err:
            if not isinstance(err, ConnectionLostError):
                self._lose("its store closed", err)
        except Exception:
            pass  # any other failure keeps renewing until the lease's own deadline

    def _run_out_at(self, deadline: float) -> None:
        if self._running_out is not None:
            self._running_out.cancel()
        ttl = self._keys._ttl / 1000  # pyright: ignore[reportPrivateUsage]
        left = max(0.0, deadline - min(1.0, ttl / 10) - time.monotonic())
        loop = asyncio.get_running_loop()
        self._running_out = loop.call_later(left, self._lose, "it was not renewed in time", None)

    def _lose(self, why: str, cause: ClosedError | None) -> None:
        self._stop()
        if self._lost is not None:
            return
        self._lost = cause or LostError(f"{self._place}: the lease was lost: {why}")
        if self._inside and self._task is not None:
            self._cancelling = self._task.cancelling()
            self._cancel_sent = True
            self._task.cancel()

    def _stop(self) -> None:
        for timer in (self._running_out, self._revoking):
            if timer is not None:
                timer.cancel()
        if self._renewing is not None and self._renewing is not asyncio.current_task():
            self._renewing.cancel()

    async def _release(self, *, failed: bool) -> None:
        """Lets the key go, saying whether the block failed, which the key's breaker counts.

        A lease that ran out meanwhile lets go nothing another holds now.
        """
        body = LocksLease.encode(lease=self._lease, failed=failed or None)
        link = self._keys._link  # pyright: ignore[reportPrivateUsage]
        with contextlib.suppress(Exception):
            await link.run("read", lambda c: c.session.call(methods["locks.release"], body))


class Lock:
    """Keys held one holder at a time: ``store.lock(name)``."""

    def __init__(self, link: Link, name: str, ttl: Duration, max_hold: Duration | None) -> None:
        self.name = name
        self._keys = _Keys(link, "lock", name, 1, ttl, max_hold)

    def hold(self, key: Key, *, wait: Duration = _WAIT) -> Holding[Lease]:
        """Waits for key in line and holds it through the block: ``async with invoices.hold(id) as lease:``.

        A key still held after wait is InUseError; a lease lost while the
        block ran is LostError, though the block succeeded.
        """
        return Holding(self._keys, key, _Asked(ms(wait, f"lock {self.name}: its wait")), Lease, False)

    def try_hold(self, key: Key) -> Holding[Lease | None]:
        """Holds key through the block if nobody holds it; the lease is None when somebody does."""
        return Holding(self._keys, key, _Asked(0), Lease, True)  # pyright: ignore[reportReturnType]


class Semaphore:
    """Keys held by up to their limit at a time: ``store.semaphore(name, limit=3)``."""

    def __init__(
        self,
        link: Link,
        name: str,
        limit: int,
        ttl: Duration,
        max_hold: Duration | None,
        background: int | None,
        breaker: Breaker | None = None,
    ) -> None:
        if isinstance(limit, bool) or not isinstance(limit, int) or not 1 <= limit <= 1000:  # pyright: ignore[reportUnnecessaryIsInstance]
            raise InvalidError(f"semaphore {name}: a limit of {limit!r}, not 1 to 1000")
        if background is not None and (
            isinstance(background, bool) or not isinstance(background, int) or not 1 <= background <= limit  # pyright: ignore[reportUnnecessaryIsInstance]
        ):
            raise InvalidError(f"semaphore {name}: a background of {background!r}, not 1 to its limit {limit}")
        breaking = _breaking(name, breaker)
        _keep_shape(link, name, (limit, background, breaking))
        self.name = name
        self.limit = limit
        self._keys = _Keys(link, "semaphore", name, limit, ttl, max_hold, background, breaking)

    async def state(self, key: Key) -> KeyState:
        """How key stands now: up or down and since when, its last failed hold, how many wait and hold it::

        state = await cabinet.state("requests")
        if not state.up:
            print(f"the cabinet is down since {state.down_since}")
        """
        return await self._keys.state(key)

    def hold(
        self, key: Key, *, wait: Duration = _WAIT, priority: Priority = "urgent", timeout: Duration | None = None
    ) -> Holding[Permit]:
        """Waits for one of key's places in line and holds it through the block.

        In the background it waits behind every urgent hold and takes no more
        than the semaphore's ``background`` places; past ``timeout`` from the
        grant the place goes to the next in line and the block ends LostError::

            async with cabinet.hold("requests", priority="background", timeout="15s"):
                await sync()
        """
        asked = self._asked(ms(wait, f"semaphore {self.name}: its wait"), priority, timeout)
        return Holding(self._keys, key, asked, Permit, False)

    def try_hold(
        self, key: Key, *, priority: Priority = "urgent", timeout: Duration | None = None
    ) -> Holding[Permit | None]:
        """Holds one of key's places through the block if one is free; None when none is."""
        return Holding(self._keys, key, self._asked(0, priority, timeout), Permit, True)  # pyright: ignore[reportReturnType]

    def _asked(self, wait: int, priority: Priority, timeout: Duration | None) -> _Asked:
        if priority not in ("urgent", "background"):
            raise InvalidError(f"semaphore {self.name}: a priority of {priority!r}, not urgent or background")
        timed = None if timeout is None else ms(timeout, f"semaphore {self.name}: its timeout")
        if timed == 0:
            raise InvalidError(f"semaphore {self.name}: a timeout of zero")
        return _Asked(wait, priority, timed)
