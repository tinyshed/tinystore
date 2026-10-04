"""Work that runs at its time, in jobs.db, as Go's jobs keeps it.

Queues ordered by time, leases, retries and repeats. A work loop is the
server's own Work, its jobs handed over the connection and settled by what
the handler returns: returning acknowledges a job and raising retries it.
"""

from __future__ import annotations

import asyncio
import contextlib
import inspect
import json
import re
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Literal, cast

from ._connection import Connection, Link, check_name, download, handle_on
from ._page import Page
from ._session import LostError
from ._time import Duration, date_of, ms, unix_ms
from ._values import from_json, to_json
from ._wire.messages import (
    METHODS,
    JobsAnswer,
    JobsBatch,
    JobsChange,
    JobsEntry,
    JobsHeld,
    JobsKept,
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
from .sql import batch_belongs_to, database_belongs_to

if TYPE_CHECKING:
    from collections.abc import AsyncIterator, Awaitable, Callable, Iterable
    from datetime import datetime

    from .sql import Database, SqlBatch

_ACK, _RETRY, _FAIL, _SNOOZE, _EXTEND, _PROGRESS = 1, 2, 3, 4, 5, 6
_STATES: dict[int, str] = {1: "waiting", 2: "running", 3: "failed", 4: "done", 5: "cancelled"}
_REPORT_EVERY = 0.1


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
    """A job as its queue holds it; state is waiting, running, failed, done or cancelled."""

    key: str
    value: V
    at: datetime | None
    attempt: int
    state: str
    error: str | None
    repeat: str | None
    ahead: int = 0
    """the jobs that run before a waiting one, up to 10,000; get and watch count it, scan leaves it 0"""
    progress: Any = None
    """what a running job's handler last reported with job.progress"""
    ran: datetime | None = None
    """when the last run a handler finished began: acknowledged, retried, failed or snoozed"""
    took: float | None = None
    """how many seconds that run took"""


def _progress_text(progress: object) -> str:
    """The JSON a handler reports of its job, at most 4 KiB of it."""
    try:
        text = json.dumps(progress, separators=(",", ":"), allow_nan=False)
    except (TypeError, ValueError) as err:
        raise InvalidError(f"a job's progress JSON cannot write: {err}") from err
    if len(text.encode()) > 4096:
        raise InvalidError("a job's progress is past 4 KiB of JSON")
    return text


class _Reporter:
    """Sends what a handler reports of its job: the latest, one send at a time and at most one each 100 ms.

    A handler reporting each chunk it reads then costs the connection ten
    frames a second.
    """

    def __init__(self, send: Callable[[str], Awaitable[None]]) -> None:
        self._send = send
        self._latest: str | None = None
        self._sent = 0.0
        self._stopping = asyncio.Event()
        self._task: asyncio.Task[None] | None = None

    def report(self, text: str) -> None:
        self._latest = text
        if self._task is None:
            self._task = asyncio.get_running_loop().create_task(self._sending())

    async def _sending(self) -> None:
        loop = asyncio.get_running_loop()
        while self._latest is not None and not self._stopping.is_set():
            wait = self._sent + _REPORT_EVERY - loop.time()
            if wait > 0:
                with contextlib.suppress(TimeoutError):
                    await asyncio.wait_for(self._stopping.wait(), wait)
                continue
            text, self._latest = self._latest, None
            self._sent = loop.time()
            with contextlib.suppress(Exception):
                await self._send(text)
        self._task = None

    async def stop(self) -> None:
        """Drops what waits and lets what is being sent finish, before the job's outcome follows it."""
        self._stopping.set()
        if self._task is not None:
            await self._task


class _Steps:
    """The steps of a held job's run, which the server keeps under the number the job was held by."""

    def __init__(self, call: Callable[[int, bytes], Awaitable[bytes]], job: int) -> None:
        self._call = call
        self._job = job

    async def run[R](self, name: str, fn: Callable[[], Awaitable[R] | R]) -> R:
        kept = JobsKept.decode(await self._call(METHODS["jobs.step"], JobsAnswer.encode(job=self._job, name=name)))
        if kept.get("found"):
            return cast("R", json.loads(kept.get("answer", "null")))
        answer = fn()
        if inspect.isawaitable(answer):
            answer = await answer
        try:
            text = json.dumps(answer)
        except (TypeError, ValueError) as err:
            raise InvalidError(f"step {name} answered what JSON cannot write: {err}") from err
        await self._call(METHODS["jobs.keep"], JobsAnswer.encode(job=self._job, name=name, answer=text))
        return cast("R", answer)


@dataclass
class Job[V]:
    """A job in a work loop's handler; retry, fail or snooze decide how it settles instead of its return.

    A cancel that takes the job while it runs cancels the handler's task.
    """

    key: str
    value: V
    at: datetime | None
    attempt: int
    settlement: dict[str, Any] | None = field(default=None, repr=False)
    _reporter: _Reporter | None = field(default=None, repr=False)
    _steps: _Steps | None = field(default=None, repr=False)

    async def step[R](self, name: str, fn: Callable[[], Awaitable[R] | R]) -> R:
        """Runs fn once in the job's run and keeps what it answers, as JSON.

        An attempt after a retry, a lost lease or a restart gets the kept
        answer back without running fn again. A step whose attempt ends before
        its answer is kept runs again, so what fn does outside the store should
        bear doing twice. A name is the step's within the run, which a loop
        numbers: "model:1", "tool:1", "model:2". An answer is at most 1 MiB of
        JSON.

            hits = await job.step("search", lambda: search(job.value["query"]))
        """
        if self._steps is None:
            raise InvalidError("a job no work loop handed over keeps no steps")
        return await self._steps.run(name, fn)

    def progress(self, progress: object) -> None:
        """Reports how far the job got, any JSON within 4 KiB, which get and watch show until it is settled.

        It does not wait for the server.
        """
        text = _progress_text(progress)
        if self._reporter is not None:
            self._reporter.report(text)

    def retry(self, error: object = None, *, at: datetime | None = None, after: Duration | None = None) -> None:
        self.settlement = {"how": _RETRY, "err": _reason(error), **_timing(at, after)}

    def fail(self, error: object = None) -> None:
        self.settlement = {"how": _FAIL, "err": _reason(error)}

    def snooze(self, *, at: datetime | None = None, after: Duration | None = None) -> None:
        """Moves the job to another time without counting the attempt."""
        self.settlement = {"how": _SNOOZE, **_timing(at, after)}


class ClaimedJob[V]:
    """A job a claim leased, which its caller settles before the lease ends, on the claim's connection."""

    def __init__(
        self,
        held: dict[str, Any],
        value: V,
        settle: Callable[[dict[str, Any]], Awaitable[None]],
        call: Callable[[int, bytes], Awaitable[bytes]],
    ) -> None:
        self.key: str = held.get("key", "")
        self.value = value
        self.at = date_of(held.get("at"))
        self.attempt: int = held.get("attempt", 0)
        self._job: int = held.get("job", 0)
        self._settle = settle
        self._steps = _Steps(call, self._job)

    async def step[R](self, name: str, fn: Callable[[], Awaitable[R] | R]) -> R:
        """Runs fn once in the job's run and keeps its answer, as a work loop's job.step does."""
        return await self._steps.run(name, fn)

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

    async def progress(self, progress: object) -> None:
        """Reports how far the job got, any JSON within 4 KiB, which get and watch show until it is settled."""
        await self._settle({"job": self._job, "how": _PROGRESS, "progress": _progress_text(progress)})


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
        max_running: int | None = None,
        in_: Database | None = None,
    ) -> Queue[V]:
        """A queue of JSON values of one type; its policy is the options' and the server's defaults.

        max_running bounds the jobs that run at once across every worker of
        the store. in_ keeps the queue in a database's file instead of jobs.db,
        so that a batch of the database commits a job with its rows::

            index = store.jobs.queue("index", IndexNote, in_=db)
            async with db.batch() as tx:
                tx.exec("update notes set body = ? where id = ?", body, note_id)
                index.with_tx(tx).enqueue(IndexNote(note_id))
        """
        check_name(name, "queue")
        fields = _policy(lease, max_attempts, backoff, max_waiting, keep_failed, keep_done, max_running)
        open_body = JobsQueue.encode(name=name, in_=None if in_ is None else in_.name, **fields)
        return Queue(self._link, name, open_body, of, in_)

    def schedule(self, name: str, repeat: Repeat, /, in_: Database | None = None, **options: Any) -> Queue[None]:
        """A queue of one job under the schedule's name, repeating as it says; work runs it, cancel stops it."""
        check_name(name, "schedule")
        fields = _policy(**options)
        database = None if in_ is None else in_.name
        open_body = JobsQueue.encode(name=name, schedule=repeat.fields(), in_=database, **fields)
        return Queue(self._link, name, open_body, None, in_)


def _policy(
    lease: Duration | None = None,
    max_attempts: int | None = None,
    backoff: tuple[Duration, Duration] | None = None,
    max_waiting: int | None = None,
    keep_failed: Duration | None = None,
    keep_done: Duration | None = None,
    max_running: int | None = None,
) -> dict[str, Any]:
    return {
        "lease": None if lease is None else ms(lease),
        "max_attempts": max_attempts,
        "backoff_first": None if backoff is None else ms(backoff[0]),
        "backoff_most": None if backoff is None else ms(backoff[1]),
        "max_waiting": max_waiting,
        "keep_failed": None if keep_failed is None else ms(keep_failed),
        "keep_done": None if keep_done is None else ms(keep_done),
        "max_running": max_running,
    }


class QueueTx[V]:
    """A queue's enqueue inside a batch of the database it lives in."""

    def __init__(self, tx: SqlBatch, open_body: bytes, job: Callable[..., dict[str, Any]]) -> None:
        self._tx, self._open, self._job = tx, open_body, job

    def enqueue(
        self,
        value: V,
        *,
        at: datetime | None = None,
        after: Duration | None = None,
        key: str | None = None,
        repeat: Repeat | None = None,
    ) -> asyncio.Future[None]:
        """Adds a job to the batch; the future settles once the batch has committed."""
        return self._tx.enqueue(self._open, self._job(value, at, after, key, repeat))


class Queue[V]:
    def __init__(self, link: Link, name: str, open_body: bytes, of: Any, in_: Database | None = None) -> None:
        if in_ is not None and not database_belongs_to(in_, link):
            raise InvalidError(f"queue {name}: its SQL database belongs to another store")
        self._link, self.name, self._open, self._of, self._in = link, name, open_body, of, in_

    def _decode(self, text: str) -> Any:
        return None if self._of is None else from_json(text, self._of)

    async def _handle(self, connection: Connection) -> int:
        """The queue's handle, its database opened first when it lives in one, as the server needs."""
        if self._in is not None:
            await self._in.handle(connection)
        return await handle_on(connection, METHODS["jobs.open"], self._open)

    def with_tx(self, tx: SqlBatch) -> QueueTx[V]:
        """The queue's enqueue inside a batch of the database it lives in, opened in_: the job commits with its rows."""
        if self._in is None or self._in.name != tx.database or not batch_belongs_to(tx, self._link):
            lives = "jobs.db" if self._in is None else f"sql {self._in.name}"
            raise InvalidError(
                f"the queue {self.name} lives in {lives}, not in sql {tx.database}: "
                "open it with in_=db to enqueue in that database's batches"
            )
        return QueueTx(tx, self._open, self._job)

    def _job(
        self, value: V, at: datetime | None, after: Duration | None, key: str | None, repeat: Repeat | None
    ) -> dict[str, Any]:
        return {
            "value": "{}" if self._of is None else to_json(value),
            "key": key,
            "at": None if at is None else unix_ms(at),
            "after": None if after is None else ms(after),
            "repeat": None if repeat is None else repeat.fields(),
        }

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
        encoded = [self._job(j.value, j.at, j.after, j.key, j.repeat) for j in jobs]

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
        """Changes a job that waits or failed; one a worker holds, done or absent is ConflictError."""
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
        """Removes the job under key, whether it waits, runs or failed, and says whether there was one.

        A running job's handler task is cancelled, and what it returns settles nothing.
        """

        async def attempt(connection: Connection) -> bool:
            handle = await self._handle(connection)
            body = await connection.session.call(METHODS["jobs.cancel"], JobsKey.encode(handle=handle, key=key))
            return bool(JobsEntry.decode(body).get("found"))

        return await self._link.run("write", attempt)

    async def get(self, key: str) -> JobEntry[V] | None:
        """The job a key names, waiting, running or failed, or done while keep_done keeps its key."""

        async def attempt(connection: Connection) -> dict[str, Any]:
            handle = await self._handle(connection)
            return JobsEntry.decode(
                await connection.session.call(METHODS["jobs.get"], JobsKey.encode(handle=handle, key=key))
            )

        entry = await self._link.run("read", attempt)
        return self._entry(entry) if entry.get("found") else None

    def _entry(self, e: dict[str, Any]) -> JobEntry[V]:
        state = _STATES.get(e.get("state", 1), "waiting")
        progress = e.get("progress")
        value: Any = None if state == "done" and "value" not in e else self._decode(e.get("value", "null"))
        return JobEntry(
            e.get("key", ""),
            value,
            date_of(e.get("at")),
            e.get("attempt", 0),
            state,
            e.get("err"),
            e.get("repeat"),
            e.get("ahead", 0),
            None if progress is None else json.loads(progress),
            date_of(e.get("ran")),
            None if "ran" not in e else e.get("took", 0) / 1000,
        )

    async def watch(self, key: str) -> AsyncIterator[JobEntry[V]]:
        """Yields the job under key as it is, then at each change, until it ends: done, failed or cancelled.

        A change is of its state, place, attempt, time, progress or error, and
        the last entry is the one that ended it. A key that names no job yields
        nothing. Leaving the loop ends the watch; a connection lost is
        connected again, and the watch goes on from the job as it is then.

            async for s in videos.watch(video_id):
                await send(s.state, s.ahead, s.progress)
        """
        last: tuple[object, ...] | None = None
        while True:

            async def attempt(connection: Connection) -> Any:
                handle = await self._handle(connection)
                opened = await connection.session.open(
                    METHODS["jobs.watch"], JobsKey.encode(handle=handle, key=key), True
                )
                await opened.next()
                return opened

            stream = await self._link.run("read", attempt)
            try:
                while True:
                    event = await stream.next()
                    stream.consumed(len(event.body))
                    if event.end:
                        return
                    entry = self._entry(JobsEntry.decode(event.body))
                    seen = (entry.state, entry.ahead, entry.attempt, entry.at, json.dumps(entry.progress), entry.error)
                    if seen != last:
                        last = seen
                        yield entry
            except LostError:
                continue
            finally:
                stream.cancel()

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

            return ClaimedJob(held, self._decode(held.get("value", "null")), settle, connection.session.call)

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
                cancels=True,
            ),
            False,
        )
        in_hand: set[asyncio.Task[None]] = set()
        by_number: dict[int, tuple[asyncio.Task[None], _Taken]] = {}
        try:
            await stream.next()
            while True:
                event = await stream.next()
                stream.consumed(len(event.body))
                if event.end:
                    return
                held = JobsHeld.decode(event.body)
                number = held.get("job", 0)
                if held.get("cancelled"):
                    running = by_number.get(number)
                    if running is not None:
                        running[1].taken = True
                        running[0].cancel()
                    continue
                taken = _Taken()
                steps = _Steps(connection.session.call, number)
                task = asyncio.ensure_future(self._run(stream, held, steps, handler, limit, taken))
                in_hand.add(task)
                by_number[number] = (task, taken)
                task.add_done_callback(in_hand.discard)
                task.add_done_callback(lambda _, n=number: by_number.pop(n, None))
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
        steps: _Steps,
        handler: Callable[[Job[V]], Awaitable[None]],
        limit: float,
        taken: _Taken,
    ) -> None:
        number = held.get("job", 0)

        async def report(progress: str) -> None:
            await stream.send(JobsOutcome.encode(job=number, how=_PROGRESS, progress=progress), False)

        reporter = _Reporter(report)
        try:
            job = Job(
                held.get("key", ""),
                self._decode(held.get("value", "null")),
                date_of(held.get("at")),
                held.get("attempt", 0),
                _reporter=reporter,
                _steps=steps,
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
                await reporter.stop()
                if taken.taken:
                    # a cancel took the job: what the handler leaves settles nothing
                    asyncio.current_task().uncancel()  # type: ignore[union-attr]
                    return
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
        await reporter.stop()
        if taken.taken:
            return
        with contextlib.suppress(Exception):
            await stream.send(JobsOutcome.encode(outcome), False)


class _Taken:
    """Whether a cancel took a job in a work loop's hands, whose handler's task it cancels."""

    taken = False
