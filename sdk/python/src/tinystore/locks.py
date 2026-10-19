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
from dataclasses import dataclass
from typing import TYPE_CHECKING, Literal

from ._connection import check_name
from ._handles import key_text
from ._session import ConnectionLostError
from ._time import ms
from ._wire.protocol import LocksHeld, LocksHold, LocksLease, LocksRenew, methods
from .errors import ClosedError, InUseError, InvalidError, LostError
from .sql import inside_database_tx
from .tx import inside_store_tx

if TYPE_CHECKING:
    from types import TracebackType
    from typing import Any

    from ._connection import Link
    from ._time import Duration
    from ._wire.codec import Key

_WAIT = "10s"

_holding: contextvars.ContextVar[tuple[tuple[int, str], ...]] = contextvars.ContextVar("tinystore holding", default=())
"""The keys the caller holds or waits for, each with the link of the store it is of."""


@dataclass(frozen=True, slots=True)
class Lease:
    """A holding of a lock's key."""

    fence: int
    """grows with every lease the store gives, 128 bits: hand it to whatever must refuse a late holder"""


@dataclass(frozen=True, slots=True)
class Permit:
    """A holding of a semaphore's key: one of its limit."""


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
    ) -> None:
        check_name(name, kind)
        self._link = link
        self._kind = kind
        self._name = name
        self._limit = limit
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

    async def ask(self, key: Key, wait: int) -> dict[str, Any]:
        """The key, once the store gives it, or that it was held for all of wait.

        A connection lost on the way asks again with the same request, which
        the store answers with the lease it gave, or with the ask's place in line.
        """
        body = LocksHold.encode(
            kind=self._kind,
            name=self._name,
            key=key_text(key),
            limit=self._limit,
            ttl=self._ttl,
            max_hold=self._max_hold,
            wait=wait,
            request=secrets.token_bytes(16),
        )
        answer = await self._link.run("read", lambda c: c.session.call(methods["locks.hold"], body))
        return LocksHeld.decode(answer)


class Holding[V]:
    """A hold of a key for the block of an ``async with``, which gives what the block holds it by.

    A lease lost while the block runs cancels it at its next await, as
    ``asyncio.timeout`` does, and the block ends LostError; one lost after its
    last await ends it LostError too, since another holder may have run beside it.
    """

    def __init__(self, keys: _Keys, key: Key, wait: int, view: type[V], tries: bool) -> None:
        self._keys = keys
        self._key = key
        self._wait = wait
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

    async def __aenter__(self) -> V:
        self._keys.refuse(self._place)
        answer = await self._keys.ask(self._key, self._wait)
        if not answer.get("acquired"):
            if self._tries:
                return None  # type: ignore[return-value]
            raise InUseError(f"{self._place}: held for all of the wait")
        self._start(answer)
        self._token = _holding.set((*_holding.get(), (id(self._keys._link), self._place)))  # pyright: ignore[reportPrivateUsage]
        self._task = asyncio.current_task()
        self._inside = True
        fence = answer.get("fence") or "0"
        return Lease(int(fence)) if self._view is Lease else Permit()  # type: ignore[return-value]

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
            await self._release()
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
            self._revoking = loop.call_later(revoke_in / 1000, self._lose, "it was revoked at its max_hold", None)

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

    async def _release(self) -> None:
        """Lets the key go: a lease that ran out meanwhile lets go nothing another holds now."""
        body = LocksLease.encode(lease=self._lease)
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
        return Holding(self._keys, key, ms(wait, f"lock {self.name}: its wait"), Lease, False)

    def try_hold(self, key: Key) -> Holding[Lease | None]:
        """Holds key through the block if nobody holds it; the lease is None when somebody does."""
        return Holding(self._keys, key, 0, Lease, True)  # pyright: ignore[reportReturnType]


class Semaphore:
    """Keys held by up to their limit at a time: ``store.semaphore(name, limit=3)``."""

    def __init__(self, link: Link, name: str, limit: int, ttl: Duration, max_hold: Duration | None) -> None:
        if isinstance(limit, bool) or not isinstance(limit, int) or not 1 <= limit <= 1000:  # pyright: ignore[reportUnnecessaryIsInstance]
            raise InvalidError(f"semaphore {name}: a limit of {limit!r}, not 1 to 1000")
        self.name = name
        self.limit = limit
        self._keys = _Keys(link, "semaphore", name, limit, ttl, max_hold)

    def hold(self, key: Key, *, wait: Duration = _WAIT) -> Holding[Permit]:
        """Waits for one of key's permits in line and holds it through the block."""
        return Holding(self._keys, key, ms(wait, f"semaphore {self.name}: its wait"), Permit, False)

    def try_hold(self, key: Key) -> Holding[Permit | None]:
        """Holds one of key's permits through the block if one is free; None when none is."""
        return Holding(self._keys, key, 0, Permit, True)  # pyright: ignore[reportReturnType]
