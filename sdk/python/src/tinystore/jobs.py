"""Work that runs at its time, in jobs.db, as Go's jobs keeps it.

Queues ordered by time, leases, retries and repeats. A work loop is the
server's own Work, its jobs handed over the connection and settled by what
the handler returns: returning acknowledges a job and raising retries it.
"""

from __future__ import annotations

import asyncio
import contextlib
import re
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Literal

from ._connection import Connection, Link, check_name, download, handle_on
from ._page import Page
from ._session import LostError
from ._time import Duration, date_of, ms, unix_ms
from ._values import from_json, to_json
from ._wire.messages import (
    METHODS,
    JobsBatch,
    JobsChange,
    JobsEntry,
    JobsHeld,
    JobsKey,
    JobsLease,
    JobsOutcome,
    JobsOutcomes,
    JobsPage,
    JobsQuery,
    JobsQueue,
    JobsSettled,
    JobsWorkers,
)
from .errors import InvalidError, error_of

if TYPE_CHECKING:
    from collections.abc import AsyncIterator, Awaitable, Callable, Iterable
    from datetime import datetime

_ACK, _RETRY, _FAIL, _SNOOZE, _EXTEND = 1, 2, 3, 4, 5
_STATES: dict[int, str] = {1: "waiting", 2: "leased", 3: "failed"}


@dataclass(frozen=True, slots=True)
class Repeat:
    """When a job runs again: cron text in a zone by its name, or every so many milliseconds."""

    cron: str | None = None
    zone: str | None = None
    every: int | None = None

    def fields(self) -> dict[str, Any]:
        return {"cron": self.cron, "zone": self.zone, "every": self.every}


def cron(expression: str, zone: str) -> Repeat:
    return Repeat(cron=expression, zone=zone)


def daily(at: str, zone: str) -> Repeat:
    """Every day at a time of the zone's clock: daily("03:10", "Europe/Moscow")."""
    found = re.fullmatch(r"(\d{1,2}):(\d{2})", at)
    if found is None or int(found[1]) > 23 or int(found[2]) > 59:
        raise InvalidError(f"the daily time {at!r}; write it as 03:10")
    return Repeat(cron=f"{int(found[2])} {int(found[1])} * * *", zone=zone)


def every(d: Duration) -> Repeat:
    return Repeat(every=ms(d))


@dataclass(frozen=True, slots=True)
class Enqueue[V]:
    """A job for enqueue_all."""

    value: V
    at: datetime | None = None
    after: Duration | None = None
    key: str | None = None
    repeat: Repeat | None = None


@dataclass(frozen=True, slots=True)
class JobEntry[V]:
    key: str
    value: V
    at: datetime | None
    attempt: int
    state: str
    error: str | None
    repeat: str | None


@dataclass
class Job[V]:
    """A job in a work loop's handler; retry, fail or snooze decide how it settles instead of its return."""

    key: str
    value: V
    at: datetime | None
    attempt: int
    settlement: dict[str, Any] | None = field(default=None, repr=False)

    def retry(self, error: object = None, *, at: datetime | None = None, after: Duration | None = None) -> None:
        self.settlement = {"how": _RETRY, "err": _reason(error), **_timing(at, after)}

    def fail(self, error: object = None) -> None:
        self.settlement = {"how": _FAIL, "err": _reason(error)}

    def snooze(self, *, at: datetime | None = None, after: Duration | None = None) -> None:
        """Moves the job to another time without counting the attempt."""
        self.settlement = {"how": _SNOOZE, **_timing(at, after)}


class ClaimedJob[V]:
    """A job a claim leased, which its caller settles before the lease ends, on the claim's connection."""

    def __init__(self, held: dict[str, Any], value: V, settle: Callable[[dict[str, Any]], Awaitable[None]]) -> None:
        self.key: str = held.get("key", "")
        self.value = value
        self.at = date_of(held.get("at"))
        self.attempt: int = held.get("attempt", 0)
        self._job: int = held.get("job", 0)
        self._settle = settle

    async def ack(self) -> None:
        await self._settle({"job": self._job, "how": _ACK})

    async def retry(self, error: object = None, *, at: datetime | None = None, after: Duration | None = None) -> None:
        await self._settle({"job": self._job, "how": _RETRY, "err": _reason(error), **_timing(at, after)})

    async def fail(self, error: object = None) -> None:
        await self._settle({"job": self._job, "how": _FAIL, "err": _reason(error)})

    async def snooze(self, *, at: datetime | None = None, after: Duration | None = None) -> None:
        await self._settle({"job": self._job, "how": _SNOOZE, **_timing(at, after)})

    async def extend(self, d: Duration) -> None:
        """Holds the job this much longer from now."""
        await self._settle({"job": self._job, "how": _EXTEND, "after": ms(d)})


def _reason(error: object) -> str:
    return "the handler gave no reason" if error is None else str(error)


def _timing(at: datetime | None, after: Duration | None) -> dict[str, Any]:
    if at is not None:
        return {"at": unix_ms(at)}
    return {} if after is None else {"after": ms(after)}


class Jobs:
    def __init__(self, link: Link) -> None:
        self._link = link

    def queue[V](
        self,
        name: str,
        of: type[V] | Any = Any,
        /,
        *,
        lease: Duration | None = None,
        max_attempts: int | None = None,
        backoff: tuple[Duration, Duration] | None = None,
        max_waiting: int | None = None,
        keep_failed: Duration | None = None,
        keep_done: Duration | None = None,
    ) -> Queue[V]:
        """A queue of JSON values of one type; its policy is the options' and the server's defaults."""
        check_name(name, "queue")
        fields = _policy(lease, max_attempts, backoff, max_waiting, keep_failed, keep_done)
        return Queue(self._link, name, JobsQueue.encode(name=name, **fields), of)

    def schedule(self, name: str, repeat: Repeat, /, **options: Any) -> Queue[None]:
        """A queue of one job under the schedule's name, repeating as it says; work runs it, cancel stops it."""
        check_name(name, "schedule")
        fields = _policy(**options)
        return Queue(self._link, name, JobsQueue.encode(name=name, schedule=repeat.fields(), **fields), None)


def _policy(
    lease: Duration | None = None,
    max_attempts: int | None = None,
    backoff: tuple[Duration, Duration] | None = None,
    max_waiting: int | None = None,
    keep_failed: Duration | None = None,
    keep_done: Duration | None = None,
) -> dict[str, Any]:
    return {
        "lease": None if lease is None else ms(lease),
        "max_attempts": max_attempts,
        "backoff_first": None if backoff is None else ms(backoff[0]),
        "backoff_most": None if backoff is None else ms(backoff[1]),
        "max_waiting": max_waiting,
        "keep_failed": None if keep_failed is None else ms(keep_failed),
        "keep_done": None if keep_done is None else ms(keep_done),
    }


class Queue[V]:
    def __init__(self, link: Link, name: str, open_body: bytes, of: Any) -> None:
        self._link, self.name, self._open, self._of = link, name, open_body, of

    def _decode(self, text: str) -> Any:
        return None if self._of is None else from_json(text, self._of)

    async def _handle(self, connection: Connection) -> int:
        return await handle_on(connection, METHODS["jobs.open"], self._open)

    async def enqueue(
        self,
        value: V,
        *,
        at: datetime | None = None,
        after: Duration | None = None,
        key: str | None = None,
        repeat: Repeat | None = None,
    ) -> None:
        """Adds a job; it returns once the job is in the file."""
        await self.enqueue_all([Enqueue(value, at, after, key, repeat)])

    async def enqueue_all(self, jobs: Iterable[Enqueue[V]]) -> None:
        """Adds jobs in one transaction, all or none; a refused one names itself as call."""
        encoded = [
            {
                "value": "{}" if self._of is None else to_json(j.value),
                "key": j.key,
                "at": None if j.at is None else unix_ms(j.at),
                "after": None if j.after is None else ms(j.after),
                "repeat": None if j.repeat is None else j.repeat.fields(),
            }
            for j in jobs
        ]

        async def attempt(connection: Connection) -> None:
            handle = await self._handle(connection)
            await connection.session.call(METHODS["jobs.enqueue"], JobsBatch.encode(handle=handle, jobs=encoded))

        await self._link.run("write", attempt)

    async def update(
        self,
        key: str,
        value: V,
        *,
        at: datetime | None = None,
        after: Duration | None = None,
        repeat: Repeat | None = None,
    ) -> None:
        """Changes a job that waits or failed; one leased, done or absent is ConflictError."""
        change = {
            "value": to_json(value),
            "key": key,
            **_timing(at, after),
            "repeat": None if repeat is None else repeat.fields(),
        }

        async def attempt(connection: Connection) -> None:
            handle = await self._handle(connection)
            await connection.session.call(METHODS["jobs.update"], JobsChange.encode(handle=handle, **change))

        await self._link.run("write", attempt)

    async def cancel(self, key: str) -> bool:
        """Removes a job that waits or failed, and says whether it came in time."""

        async def attempt(connection: Connection) -> bool:
            handle = await self._handle(connection)
            body = await connection.session.call(METHODS["jobs.cancel"], JobsKey.encode(handle=handle, key=key))
            return bool(JobsEntry.decode(body).get("found"))

        return await self._link.run("write", attempt)

    async def get(self, key: str) -> JobEntry[V] | None:
        async def attempt(connection: Connection) -> dict[str, Any]:
            handle = await self._handle(connection)
            return JobsEntry.decode(
                await connection.session.call(METHODS["jobs.get"], JobsKey.encode(handle=handle, key=key))
            )

        entry = await self._link.run("read", attempt)
        return self._entry(entry) if entry.get("found") else None

    def _entry(self, e: dict[str, Any]) -> JobEntry[V]:
        return JobEntry(
            e.get("key", ""),
            self._decode(e.get("value", "null")),
            date_of(e.get("at")),
            e.get("attempt", 0),
            _STATES.get(e.get("state", 1), "waiting"),
            e.get("err"),
            e.get("repeat"),
        )

    async def scan(
        self,
        *,
        prefix: str | None = None,
        state: Literal["failed"] | None = None,
        after: str | None = None,
        limit: int | None = None,
    ) -> Page[JobEntry[V], str]:
        """A page of the jobs under a prefix in the byte order of their keys, and where the next begins."""

        async def attempt(connection: Connection) -> tuple[list[bytes], bytes]:
            handle = await self._handle(connection)
            query = JobsQuery.encode(
                handle=handle,
                prefix=prefix,
                state=3 if state == "failed" else None,
                after=after,
                limit=limit,
            )
            got = await download(connection, METHODS["jobs.scan"], query)
            return got.items, got.trailer

        items, trailer = await self._link.run("read", attempt)
        page = JobsPage.decode(trailer)
        entries = [self._entry(JobsEntry.decode(i)) for i in items]
        return Page(entries, page.get("after", "") if page.get("more") else None)

    async def all(
        self,
        *,
        prefix: str | None = None,
        state: Literal["failed"] | None = None,
        limit: int | None = None,
    ) -> AsyncIterator[JobEntry[V]]:
        after: str | None = None
        while True:
            entries, after = await self.scan(prefix=prefix, state=state, after=after, limit=limit)
            for entry in entries:
                yield entry
            if after is None:
                return

    async def claim(self, *, lease: Duration | None = None) -> ClaimedJob[V] | None:
        """Leases the next due job, or None, without waiting."""

        async def attempt(connection: Connection) -> ClaimedJob[V] | None:
            handle = await self._handle(connection)
            body = JobsLease.encode(handle=handle, lease=None if lease is None else ms(lease))
            held = JobsHeld.decode(await connection.session.call(METHODS["jobs.claim"], body))
            if not held.get("found"):
                return None

            async def settle(outcome: dict[str, Any]) -> None:
                answer = await connection.session.call(METHODS["jobs.settle"], JobsOutcomes.encode(outcomes=[outcome]))
                refused = JobsSettled.decode(answer).get("settled", [None])[0]
                if refused is not None:
                    raise error_of(
                        refused.get("code", "internal"),
                        refused.get("message", ""),
                        refused.get("what"),
                    )

            return ClaimedJob(held, self._decode(held.get("value", "null")), settle)

        return await self._link.run("write", attempt)

    async def work(
        self,
        handler: Callable[[Job[V]], Awaitable[None]],
        *,
        workers: int = 1,
        timeout: Duration | None = None,
        until_idle: bool = False,
    ) -> None:
        """Runs the queue's jobs as they come due, workers at once, until the task is cancelled.

        The server's own Work loop claims ahead and extends leases, and nothing
        polls. A lost connection takes the jobs in hand with it, as a process
        that died would, and the loop connects again. Cancelled, the loop gives
        the jobs its handlers did not finish back without counting them.
        """
        while True:
            try:
                await self._link.run("read", lambda c: self._work_on(c, handler, workers, timeout, until_idle))
            except LostError:
                continue
            if until_idle:
                return

    async def _work_on(
        self,
        connection: Connection,
        handler: Callable[[Job[V]], Awaitable[None]],
        workers: int,
        timeout: Duration | None,
        until_idle: bool,
    ) -> None:
        handle = await self._handle(connection)
        limit = 60.0 if timeout is None else ms(timeout) / 1000
        stream = await connection.session.open(
            METHODS["jobs.work"],
            JobsWorkers.encode(
                handle=handle,
                workers=workers,
                timeout=round(limit * 1000),
                until_idle=until_idle or None,
            ),
            False,
        )
        in_hand: set[asyncio.Task[None]] = set()
        try:
            await stream.next()
            while True:
                event = await stream.next()
                stream.consumed(len(event.body))
                if event.end:
                    return
                task = asyncio.ensure_future(self._run(stream, JobsHeld.decode(event.body), handler, limit))
                in_hand.add(task)
                task.add_done_callback(in_hand.discard)
        except asyncio.CancelledError:
            for task in in_hand:
                task.cancel()
            await asyncio.gather(*in_hand, return_exceptions=True)
            with contextlib.suppress(Exception):
                await stream.send(b"", True)
            raise

    async def _run(
        self,
        stream: Any,
        held: dict[str, Any],
        handler: Callable[[Job[V]], Awaitable[None]],
        limit: float,
    ) -> None:
        number = held.get("job", 0)
        try:
            job = Job(
                held.get("key", ""),
                self._decode(held.get("value", "null")),
                date_of(held.get("at")),
                held.get("attempt", 0),
            )
        except Exception as err:
            outcome: dict[str, Any] = {
                "job": number,
                "how": _FAIL,
                "err": f"the value no longer reads: {err}",
            }
        else:
            try:
                await asyncio.wait_for(handler(job), limit)
                outcome = {"job": number, "how": _ACK, **(job.settlement or {})}
            except asyncio.CancelledError:
                # stopped by the loop's end: given back without counting the attempt
                outcome = {"job": number, "how": _SNOOZE, "at": held.get("at")}
                with contextlib.suppress(Exception):
                    await stream.send(JobsOutcome.encode(outcome), False)
                raise
            except Exception as err:
                outcome = {
                    "job": number,
                    "how": _RETRY,
                    "err": _reason(err),
                    **(job.settlement or {}),
                }
                if job.settlement is not None:
                    outcome = {"job": number, **job.settlement}
        with contextlib.suppress(Exception):
            await stream.send(JobsOutcome.encode(outcome), False)
