"""Work an application must do later, or now but outside the request that asked for it.

As plan/api/jobs.md has it: queues of JSON values run in the order of their
time, ids that find a job again, repeats, schedules and limits. A worker's
handlers run here while the store's loop claims the queue's jobs and hands
each over on one stream, the answers going back on it::

    reminders = store.queue("reminders", Reminder)
    await reminders.add(Reminder(user_id=42, text="Call mom"), delay="1h")

    async def remind(reminder: Reminder, run: tinystore.Run) -> None:
        await push(reminder.user_id, reminder.text)

    worker = reminders.work(remind)
    await worker.stop()                     # at shutdown
"""

from __future__ import annotations

import asyncio
import contextlib
import inspect
import json
import time
from dataclasses import dataclass, field
from datetime import datetime
from typing import TYPE_CHECKING, Any, Literal, Self, cast

from ._background import FailureLog, message_of
from ._connection import check_name
from ._handles import Home, Opener, Via, handle_on, via_link
from ._session import ConnectionLostError, Stream
from ._time import Duration, moment, ms, unix_ms
from ._values import from_json, to_json
from ._wire.protocol import (
    JobsAnswer,
    JobsCall,
    JobsChanged,
    JobsHeld,
    JobsId,
    JobsJob,
    JobsKept,
    JobsList,
    JobsPage,
    JobsQueueOpen,
    JobsScheduleOpen,
    JobsStep,
    JobsTx,
    JobsWork,
    methods,
)
from .errors import CorruptError, InvalidError, UnavailableError
from .limits import rate_of

if TYPE_CHECKING:
    from collections.abc import AsyncIterator, Awaitable, Callable
    from types import TracebackType

    from ._connection import Connection, Idempotence, Link

JobState = Literal["scheduled", "waiting", "running", "done", "failed", "cancelled"]
"""done while dedupe keeps its id; cancelled only a watcher sees"""

_MOST_HANDLERS = 1024
"""The handlers a client's worker runs at once at most, as the store bounds them."""

_FIRST_PAUSE, _LAST_PAUSE = 0.1, 30.0
"""The pause before a worker dials again after a failure, doubling up to the last."""


@dataclass(frozen=True, slots=True)
class Concurrency:
    """How many jobs of a queue run at once: in all, across every worker of the store, and in each group."""

    total: int | None = None
    group: int | None = None
    """at most this many jobs of one group at once: one customer's backlog holds back no other"""


@dataclass(frozen=True, slots=True)
class Backoff:
    """The wait before a retry: initial, doubling each time up to max, a tenth longer or shorter at random."""

    initial: Duration
    max: Duration


@dataclass(frozen=True, slots=True)
class LastRun:
    """When the last run a handler finished started and ended."""

    started_at: datetime
    ended_at: datetime


@dataclass(frozen=True, slots=True)
class Job[T]:
    """A job as its queue holds it."""

    id: str | None
    value: T
    state: JobState
    at: datetime
    """when it runs next; for one done or failed, when its last run was for"""
    attempt: int
    """its attempts, a running one included"""
    ahead: int
    """the jobs that run before a waiting one, up to 10,000"""
    progress: Any
    """what its handler last reported of a running job"""
    error: str | None
    """why its last run failed"""
    group: str | None
    repeat: str | None
    """a repeating job's repeat as it keeps it: '@every 30s +6178ms', '10 3 * * * Europe/Berlin'"""
    last_run: LastRun | None


@dataclass(frozen=True, slots=True)
class JobPage[T]:
    """A page of a queue's jobs, and where the next starts when there is one."""

    jobs: list[Job[T]]
    next: str | None


class Outcome:
    """How a run ends otherwise than done, made by run.retry, run.snooze or run.fail: its handler returns it."""

    __slots__ = ("_how",)

    def __init__(self, how: str) -> None:
        self._how = how

    def __repr__(self) -> str:
        return f"run.{self._how}(...)"


@dataclass(slots=True)
class _RunState:
    """What a run shares with its worker: whether it was told to stop, and the answers it made."""

    stopped: bool = False
    made: dict[int, tuple[Outcome, dict[str, Any]]] = field(default_factory=dict[int, tuple[Outcome, dict[str, Any]]])


class Run:
    """This call on a job: what it is, its steps and progress, and the answers its handler returns.

    A cancel that takes the job while it runs cancels its handler's task; so
    does its timeout, after which ``stopped`` is True.
    """

    def __init__(self, held: dict[str, Any], steps: _Steps, reporter: _Reporter, state: _RunState) -> None:
        self.id: str | None = held.get("id")
        """the job's id, None for one added without"""
        self.at: datetime = moment(held.get("at", 0))
        """when the run was due, which "thirty days ago" counts from even when it starts late"""
        self.attempt: int = held.get("attempt", 0)
        """this run's number among the job's attempts, the first being 1"""
        self.group: str | None = held.get("group")
        self._steps = steps
        self._reporter = reporter
        self._state = state

    @property
    def stopped(self) -> bool:
        """Whether the run passed its timeout or a cancel took its job: the handler should stop too."""
        return self._state.stopped

    async def step[R](self, name: str, fn: Callable[[], Awaitable[R] | R]) -> R:
        """Runs fn once in the job's run and keeps its answer, as JSON, across the job's attempts.

        The attempt after a retry, a lost worker or a restart gets the answer
        back without running fn again. A step whose attempt ends before its
        answer is kept runs again, so what fn does outside the store should
        bear doing twice. A name is the step's within its job, numbered in a
        loop: "model:1", "tool:1".
        """
        return await self._steps.run(name, fn)

    def set_progress(self, progress: Any) -> None:
        """Shows how far the job got, any JSON within 4 KiB: the latest goes, ten a second at most."""
        self._reporter.report(_progress_text(progress))

    def retry(self, when: Duration | datetime | None = None) -> Outcome:
        """Runs the job again after its backoff, or at the time given, the run counted as an attempt: return it."""
        return self._make("retry", {} if when is None else _timing(when))

    def snooze(self, when: Duration | datetime) -> Outcome:
        """Runs the job again at the time given, the run not counted: return it."""
        return self._make("snooze", _timing(when))

    def fail(self, reason: str | BaseException) -> Outcome:
        """Fails the job for good now, its reason kept as its error: return it."""
        return self._make("fail", {"error": message_of(reason)})

    def _make(self, how: str, fields: dict[str, Any]) -> Outcome:
        outcome = Outcome(how)
        self._state.made[id(outcome)] = (outcome, {**fields, "how": how})
        return outcome


def _answer_of(returned: object, state: _RunState) -> dict[str, Any]:
    """How a handler's return ends its run: an answer it made, done for anything else.

    An answer made and not returned fails the run, since a snooze lost for a
    missing return would otherwise mark the job done.
    """
    if isinstance(returned, Outcome):
        made = state.made.get(id(returned))
        if made is None or made[0] is not returned:
            return {"how": "retry", "error": "the handler returned an answer another run made"}
        return made[1]
    for outcome, _ in state.made.values():
        return {
            "how": "retry",
            "error": f"the handler made {outcome!r} and returned something else: return the answer that ends the run",
        }
    return {"how": "done"}


def _timing(when: Duration | datetime) -> dict[str, Any]:
    """A retry's or a snooze's time: a datetime at it, a span from now on the store's clock."""
    if isinstance(when, datetime):
        return {"at": unix_ms(when, "a run's time")}
    return {"delay": ms(when, "a run's delay")}


def _json_text(value: Any, what: str) -> str:
    try:
        return to_json(value)
    except InvalidError as err:
        raise InvalidError(f"{what}: {err}") from None


def _progress_text(progress: Any) -> str:
    text = _json_text(progress, "a job's progress")
    if len(text.encode()) > 4096:
        raise InvalidError("a job's progress past 4 KiB of JSON")
    return text


class _Reporter:
    """Sends what a handler reports of its job: the latest, one send at a time and one each 100 ms at most."""

    def __init__(self, send: Callable[[str], Awaitable[None]]) -> None:
        self._send = send
        self._latest: str | None = None
        self._sending: asyncio.Task[None] | None = None
        self._timer: asyncio.TimerHandle | None = None
        self._sent = 0.0
        self._stopped = False

    def report(self, text: str) -> None:
        self._latest = text
        self._flush()

    def _flush(self) -> None:
        if self._stopped or self._sending is not None or self._latest is None:
            return
        wait = self._sent + 0.1 - time.monotonic()
        if wait > 0:
            if self._timer is None:
                self._timer = asyncio.get_running_loop().call_later(wait, self._timed)
            return
        text, self._latest = self._latest, None
        self._sent = time.monotonic()
        self._sending = asyncio.ensure_future(self._send_one(text))

    def _timed(self) -> None:
        self._timer = None
        self._flush()

    async def _send_one(self, text: str) -> None:
        with contextlib.suppress(Exception):
            await self._send(text)
        self._sending = None
        self._flush()

    async def stop(self) -> None:
        """Drops what waits and lets what is being sent finish, before the run's answer follows it."""
        self._stopped = True
        if self._timer is not None:
            self._timer.cancel()
        if self._sending is not None:
            await self._sending


class _Steps:
    """The steps of a held job's run, which the store keeps under the run's number on its connection."""

    def __init__(self, connection: Connection, run: int) -> None:
        self._connection = connection
        self._run = run

    async def run[R](self, name: str, fn: Callable[[], Awaitable[R] | R]) -> R:
        asked = JobsStep.encode(run=self._run, name=name)
        kept = JobsKept.decode(await self._connection.session.call(methods["jobs.step"], asked))
        if kept.get("found"):
            return cast("R", json.loads(kept.get("answer", "null")))
        answered = fn()
        answer = cast("R", await answered) if inspect.isawaitable(answered) else answered
        text = _json_text(answer, f"step {name!r}'s answer")
        await self._connection.session.call(
            methods["jobs.keep"], JobsStep.encode(run=self._run, name=name, answer=text)
        )
        return answer


@dataclass(slots=True)
class Source[T]:
    """What a queue's or a schedule's calls and workers need of it."""

    link: Link
    describe: str
    """'jobs queue emails', which an error names"""
    open_method: int
    opener: Opener
    via: Via
    of: Any
    """the type its values read back as"""
    timeout: float
    """a run's timeout, in seconds"""
    workers: set[Worker]
    """the store's running workers, which its close stops"""
    home: Home | None = None
    """the database whose file keeps the queue; the store's jobs.db when None"""

    def encode(self, value: T) -> str:
        return _json_text(value, f"{self.describe}: a value")

    def decode(self, text: str) -> T:
        return cast("T", from_json(text, self.of))

    async def call(self, method: str, body: Callable[[int], bytes], idempotence: Idempotence) -> bytes:
        return await self.via.call(self.open_method, self.opener, methods[method], body, idempotence)


def _job_of[T](source: Source[T], found: dict[str, Any]) -> Job[T]:
    """A job as the store sent it, its value read as the queue reads one."""
    try:
        value = source.decode(found.get("value", "null"))
    except CorruptError as err:
        raise CorruptError(f"{source.describe}: id {found.get('id', '')!r}: {err}") from None
    started = found.get("started_at")
    progress = found.get("progress")
    return Job(
        id=found.get("id"),
        value=value,
        state=found.get("state", "waiting"),
        at=moment(found.get("at", 0)),
        attempt=found.get("attempt", 0),
        ahead=found.get("ahead", 0),
        progress=None if progress is None else json.loads(progress),
        error=found.get("error"),
        group=found.get("group"),
        repeat=found.get("repeat"),
        last_run=None if started is None else LastRun(moment(started), moment(found.get("ended_at", started))),
    )


async def _get[T](source: Source[T], job_id: str) -> Job[T] | None:
    found = JobsJob.decode(await source.call("jobs.get", lambda h: JobsId.encode(handle=h, id=job_id), "read"))
    return _job_of(source, found) if found.get("found") else None


async def _cancel(source: Source[Any], job_id: str) -> bool:
    body = await source.call("jobs.cancel", lambda h: JobsId.encode(handle=h, id=job_id), "write")
    return JobsChanged.decode(body).get("changed", False)


def _shows_the_same(a: dict[str, Any], b: dict[str, Any]) -> bool:
    """Whether two reports show a watcher the same: the job's state, place, attempt, time, progress and error."""
    return all(a.get(k) == b.get(k) for k in ("state", "ahead", "attempt", "at", "progress", "error"))


def job_fields(
    *,
    at: datetime | None = None,
    delay: Duration | None = None,
    group: str | None = None,
    every: Duration | None = None,
    cron: str | None = None,
    time_zone: str | None = None,
) -> dict[str, Any]:
    """A job's time, group and repeat, as an add, a set and an update take them."""
    return {
        "at": None if at is None else unix_ms(at, "a job's time"),
        "delay": None if delay is None else ms(delay, "a job's delay"),
        "group": group,
        "every": None if every is None else ms(every, "a job's repeat"),
        "cron": cron,
        "time_zone": time_zone,
    }


class Queue[T]:
    """A queue of jobs of one type, run in the order of their time: open it with ``store.queue(name, type)``."""

    def __init__(self, source: Source[T], name: str) -> None:
        self.name = name
        self._source = source

    @property
    def source(self) -> Source[T]:
        return self._source

    async def add(
        self,
        value: T,
        *,
        id: str | None = None,
        at: datetime | None = None,
        delay: Duration | None = None,
        group: str | None = None,
        every: Duration | None = None,
        cron: str | None = None,
        time_zone: str | None = None,
    ) -> bool:
        """Adds a job unless its id is taken, and says whether it added one.

        An id is taken by a job scheduled, waiting, running or failed, or done
        while the queue's dedupe keeps it. It returns once the job is on disk;
        a job without an id is always added.
        """
        timing = job_fields(at=at, delay=delay, group=group, every=every, cron=cron, time_zone=time_zone)
        fields = {**timing, "id": id, "value": self._source.encode(value)}
        return await self._changed("jobs.add", fields)

    async def set(
        self,
        id: str,
        value: T,
        *,
        at: datetime | None = None,
        delay: Duration | None = None,
        group: str | None = None,
        every: Duration | None = None,
        cron: str | None = None,
        time_zone: str | None = None,
    ) -> None:
        """Makes the id's job this value at this time, whatever it was.

        One waiting is made new, a failed one starts again, and a running one
        runs once more after this run, with this value.
        """
        timing = job_fields(at=at, delay=delay, group=group, every=every, cron=cron, time_zone=time_zone)
        fields = {**timing, "id": id, "value": self._source.encode(value)}
        await self._source.call("jobs.set", lambda h: JobsCall.encode(fields, handle=h), "write")

    async def update(
        self,
        id: str,
        value: T,
        *,
        at: datetime | None = None,
        delay: Duration | None = None,
        group: str | None = None,
        every: Duration | None = None,
        cron: str | None = None,
        time_zone: str | None = None,
    ) -> bool:
        """Changes the id's job when it has not started, and says whether it did."""
        timing = job_fields(at=at, delay=delay, group=group, every=every, cron=cron, time_zone=time_zone)
        fields = {**timing, "id": id, "value": self._source.encode(value)}
        return await self._changed("jobs.update", fields)

    async def cancel(self, id: str) -> bool:
        """Takes the job under id, whatever its state, and says whether there was one.

        A running one's handler task is cancelled, and what it returns then
        settles nothing; a repeating one stops.
        """
        return await _cancel(self._source, id)

    async def get(self, id: str) -> Job[T] | None:
        """Where the job under id is; None for an id with no job."""
        return await _get(self._source, id)

    async def watch(self, id: str) -> AsyncIterator[Job[T]]:
        """The job under id as it is, then again each time it changes; it ends with the job.

        Done, failed, or cancelled, a state only a watcher sees, end it. An id
        with no job yields nothing. A watch whose connection is lost watches
        again on the next; a job that ended meanwhile ends it without its last
        state.
        """
        source = self._source
        last: dict[str, Any] | None = None
        while True:
            stream = await source.link.run("read", lambda connection: _open_watch(connection, source, id))
            try:
                while True:
                    event = await stream.next()
                    stream.consumed(len(event.body))
                    if event.end:
                        return
                    found = JobsJob.decode(event.body)
                    if last is None or not _shows_the_same(last, found):
                        last = found
                        yield _job_of(source, found)
            except (ConnectionLostError, UnavailableError):
                continue
            finally:
                stream.cancel()

    async def list(
        self,
        *,
        prefix: str | None = None,
        state: Literal["scheduled", "waiting", "running", "failed"] | None = None,
        after: str | None = None,
        limit: int | None = None,
    ) -> JobPage[T]:
        """A page of the queue's jobs whose ids start with a prefix, or of its failed ones, the last first."""
        body = await self._source.call(
            "jobs.list",
            lambda h: JobsList.encode(handle=h, prefix=prefix, state=state, after=after, limit=limit),
            "read",
        )
        page = JobsPage.decode(body)
        return JobPage([_job_of(self._source, found) for found in page.get("jobs", [])], page.get("next"))

    async def all(
        self,
        *,
        prefix: str | None = None,
        state: Literal["scheduled", "waiting", "running", "failed"] | None = None,
        limit: int | None = None,
    ) -> AsyncIterator[Job[T]]:
        """Every job the options name, a page at a time, holding nothing between pages."""
        after: str | None = None
        while True:
            page = await self.list(prefix=prefix, state=state, after=after, limit=limit)
            for job in page.jobs:
                yield job
            if page.next is None:
                return
            after = page.next

    def work(self, handler: Callable[[T, Run], Awaitable[Outcome | None]], *, concurrency: int | None = None) -> Worker:
        """Runs handler on each job as it falls due, and returns the running worker at once.

        As many run at once as the queue's concurrency lets, one when it sets
        none, or concurrency under it. Stop it at shutdown, as store.close()
        does. A worker whose connection is lost reaches the store again, the
        jobs it held running again as a dead worker's would.
        """
        n = _handlers(concurrency, self._source.describe)
        return Worker(lambda working: _keep_working(self._source, handler, n, working), self._source.workers)

    async def run_due(
        self, handler: Callable[[T, Run], Awaitable[Outcome | None]], *, concurrency: int | None = None
    ) -> int:
        """Runs what is due when it starts and what falls due meanwhile, then says how many jobs ran.

        It never waits for a later job. Tests, scripts and commands use it.
        """
        return await _run_due(self._source, handler, _handlers(concurrency, self._source.describe))

    async def _changed(self, method: str, fields: dict[str, Any]) -> bool:
        body = await self._source.call(method, lambda h: JobsCall.encode(fields, handle=h), "write")
        return JobsChanged.decode(body).get("changed", False)


async def _open_watch(connection: Connection, source: Source[Any], job_id: str) -> Stream:
    handle = await handle_on(connection, source.open_method, source.opener)
    stream = await connection.session.open(methods["jobs.watch"], JobsId.encode(handle=handle, id=job_id), True)
    await stream.next()
    return stream


@dataclass(slots=True)
class JobOp:
    """A write the store's transaction keeps for its commit: its method, and its call or the id it cancels."""

    source: Source[Any]
    method: str
    call: dict[str, Any] | None = None
    id: str | None = None


class StoreTxQueue[T]:
    """A queue's writes inside the store's transaction, from ``tx.with_(queue)`` in ``store.tx``.

    Each is kept for the commit, which makes them all or none, so what one
    did is known once the transaction has committed.
    """

    def __init__(self, queue: Queue[T], keep: Callable[[JobOp], None]) -> None:
        self.name = queue.name
        self._source = queue.source
        self._keep = keep

    async def add(
        self,
        value: T,
        *,
        id: str | None = None,
        at: datetime | None = None,
        delay: Duration | None = None,
        group: str | None = None,
        every: Duration | None = None,
        cron: str | None = None,
        time_zone: str | None = None,
    ) -> None:
        """Adds a job unless its id is taken, with the commit."""
        timing = job_fields(at=at, delay=delay, group=group, every=every, cron=cron, time_zone=time_zone)
        self._write("jobs.add", {**timing, "id": id}, value)

    async def set(
        self,
        id: str,
        value: T,
        *,
        at: datetime | None = None,
        delay: Duration | None = None,
        group: str | None = None,
        every: Duration | None = None,
        cron: str | None = None,
        time_zone: str | None = None,
    ) -> None:
        """Makes the id's job this value at this time, whatever it was, with the commit."""
        timing = job_fields(at=at, delay=delay, group=group, every=every, cron=cron, time_zone=time_zone)
        self._write("jobs.set", {**timing, "id": id}, value)

    async def update(
        self,
        id: str,
        value: T,
        *,
        at: datetime | None = None,
        delay: Duration | None = None,
        group: str | None = None,
        every: Duration | None = None,
        cron: str | None = None,
        time_zone: str | None = None,
    ) -> None:
        """Changes the id's job when it has not started, with the commit."""
        timing = job_fields(at=at, delay=delay, group=group, every=every, cron=cron, time_zone=time_zone)
        self._write("jobs.update", {**timing, "id": id}, value)

    async def cancel(self, id: str) -> None:
        """Takes the job under id, whatever its state, with the commit."""
        self._keep(JobOp(self._source, "jobs.cancel", id=id))

    def _write(self, method: str, fields: dict[str, Any], value: T) -> None:
        self._keep(JobOp(self._source, method, call={**fields, "value": self._source.encode(value)}))


async def commit_jobs(link: Link, ops: list[JobOp]) -> None:
    """Sends a transaction's writes of jobs as one jobs.tx: all of them are made, or none."""

    async def attempt(connection: Connection) -> bytes:
        writes: list[dict[str, Any]] = []
        for op in ops:
            handle = await handle_on(connection, op.source.open_method, op.source.opener)
            writes.append(
                {
                    "method": methods[op.method],
                    "call": None if op.call is None else {**op.call, "handle": handle},
                    "id": None if op.id is None else {"handle": handle, "id": op.id},
                }
            )
        return await connection.session.call(methods["jobs.tx"], JobsTx.encode(writes=writes))

    await link.run("write", attempt)


class Schedule:
    """A repeat the code owns: one repeating job under its name, and the worker that runs it."""

    def __init__(self, source: Source[None], name: str, handler: Callable[[Run], Awaitable[Outcome | None]]) -> None:
        self.name = name
        self._source = source

        async def each(_: None, run: Run) -> Outcome | None:
            return await handler(run)

        self._handler = each
        self._worker = Worker(lambda working: _keep_working(source, each, None, working), source.workers)

    async def get(self) -> Job[None] | None:
        """When it runs next and how its last run went: what a page of cron jobs shows."""
        return await _get(self._source, self.name)

    async def run_due(self) -> int:
        """Runs the schedule's job when it is due, as a queue's run_due does."""
        return await _run_due(self._source, self._handler, None)

    async def stop(self) -> None:
        """Stops the schedule's worker, as a worker's stop does."""
        await self._worker.stop()

    async def cancel(self) -> bool:
        """Removes the schedule's job, until the program opens it again."""
        return await _cancel(self._source, self.name)


def _whole(n: int | None, describe: str, what: str) -> int | None:
    if n is not None and (isinstance(n, bool) or not isinstance(n, int) or n < 0):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"{describe}: {what} of {n!r}, which is no whole number")
    return n


def _backoff(backoff: Backoff | None) -> dict[str, int] | None:
    if backoff is None:
        return None
    return {"initial": ms(backoff.initial, "a backoff's initial"), "max": ms(backoff.max, "a backoff's max")}


def _handlers(n: int | None, describe: str) -> int | None:
    if n is not None and (isinstance(n, bool) or not isinstance(n, int) or not 1 <= n <= _MOST_HANDLERS):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"{describe}: a worker of {n!r} handlers, not 1 to {_MOST_HANDLERS}")
    return n


@dataclass(frozen=True, slots=True)
class QueueOptions:
    """A queue's options, as store.queue takes them."""

    attempts: int | None = None
    backoff: Backoff | None = None
    timeout: Duration | None = None
    concurrency: int | Concurrency | None = None
    rate: str | None = None
    dedupe: Duration | None = None
    keep: Duration | None = None
    max_waiting: int | None = None


def queue_fields(name: str, describe: str, options: QueueOptions) -> dict[str, Any]:
    """A queue's open, checked before anything leaves."""
    check_name(name, "queue")
    concurrency = options.concurrency
    if isinstance(concurrency, int):
        concurrency = Concurrency(total=concurrency)
    rate = None
    if options.rate is not None:
        count, per = rate_of(options.rate)
        rate = {"count": count, "per": per}
    return {
        "name": name,
        "attempts": _whole(options.attempts, describe, "attempts"),
        "backoff": _backoff(options.backoff),
        "timeout": None if options.timeout is None else ms(options.timeout, f"{describe}: timeout"),
        "concurrency": None
        if concurrency is None
        else {
            "total": _whole(concurrency.total, describe, "a concurrency"),
            "group": _whole(concurrency.group, describe, "a group's concurrency"),
        },
        "rate": rate,
        "dedupe": None if options.dedupe is None else ms(options.dedupe, f"{describe}: dedupe"),
        "keep": None if options.keep is None else ms(options.keep, f"{describe}: keep"),
        "max_waiting": _whole(options.max_waiting, describe, "a max_waiting"),
    }


def open_queue(
    link: Link, workers: set[Worker], name: str, of: Any, options: QueueOptions, home: Home | None = None
) -> Queue[Any]:
    """A queue, its open checked before anything leaves.

    home is the database whose file keeps it, its open naming the database's
    handle on each connection; the store's jobs.db when None.
    """
    describe = f"jobs queue {name}" if home is None else f"{home.describe}: jobs queue {name}"
    fields = queue_fields(name, describe, options)
    opener: Opener = JobsQueueOpen.encode(fields)
    via = via_link(link)
    if home is not None:

        async def make(connection: Connection) -> bytes:
            return JobsQueueOpen.encode(fields, database=await home.handle(connection))

        opener = home.keep(methods["jobs.queue.open"], opener, make)
        via = home.via(f"jobs queue {name}")
    timeout = 60.0 if options.timeout is None else ms(options.timeout) / 1000
    source: Source[Any] = Source(link, describe, methods["jobs.queue.open"], opener, via, of, timeout, workers, home)
    return Queue(source, name)


def open_schedule(
    link: Link,
    workers: set[Worker],
    name: str,
    handler: Callable[[Run], Awaitable[Outcome | None]],
    *,
    every: Duration | None,
    cron: str | None,
    time_zone: str | None,
    attempts: int | None,
    backoff: Backoff | None,
    timeout: Duration | None,
) -> Schedule:
    """A schedule, its open checked before anything leaves, and its worker started."""
    check_name(name, "schedule")
    describe = f"jobs schedule {name}"
    if (every is None) == (cron is None):
        raise InvalidError(f"{describe}: a schedule runs every span or by a cron, one of them")
    if cron is not None and not time_zone:
        raise InvalidError(f"{describe}: a cron needs a time_zone, 'UTC' among them")
    opener = JobsScheduleOpen.encode(
        name=name,
        every=None if every is None else ms(every, f"{describe}: every"),
        cron=cron,
        time_zone=time_zone,
        attempts=_whole(attempts, describe, "attempts"),
        backoff=_backoff(backoff),
        timeout=None if timeout is None else ms(timeout, f"{describe}: timeout"),
    )
    seconds = 60.0 if timeout is None else ms(timeout) / 1000
    source: Source[None] = Source(
        link, describe, methods["jobs.schedule.open"], opener, via_link(link), None, seconds, workers
    )
    return Schedule(source, name, handler)


@dataclass
class _Working:
    """A worker's state, which its loop and its stop share."""

    stopping: bool = False
    stream: Stream | None = None
    """the stream the worker runs on now, which a stop is sent on"""
    wake: asyncio.Event = field(default_factory=asyncio.Event)
    """wakes a loop pausing before it dials again"""


class Worker:
    """A worker running a queue's handlers here while the store's loop claims their jobs."""

    def __init__(self, run: Callable[[_Working], Awaitable[None]], workers: set[Worker]) -> None:
        self._working = _Working()
        self._workers = workers
        workers.add(self)
        self._done = asyncio.ensure_future(run(self._working))
        self._done.add_done_callback(lambda _: workers.discard(self))

    async def stop(self) -> None:
        """Takes no job more and waits for the handlers under way, each bounded by its timeout.

        The store gives back the jobs it held for them, and ends the worker
        once their answers are written.
        """
        working = self._working
        if not working.stopping:
            working.stopping = True
            if working.stream is not None:
                with contextlib.suppress(Exception):
                    await working.stream.send(JobsAnswer.encode(how="stop"), False)
            working.wake.set()
        await asyncio.shield(self._done)

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(
        self, kind: type[BaseException] | None, error: BaseException | None, trace: TracebackType | None
    ) -> None:
        await self.stop()


async def _keep_working[T](
    source: Source[T],
    handler: Callable[[T, Run], Awaitable[Outcome | None]],
    concurrency: int | None,
    working: _Working,
) -> None:
    """Runs a worker until it stops: a stream on the link's connection, and another once one is lost."""
    failures = FailureLog(f"{source.describe} worker")
    pause = _FIRST_PAUSE
    while not working.stopping:
        try:
            await source.link.run(
                "read", lambda connection: _serve(connection, source, handler, concurrency, working, until_idle=False)
            )
            failures.succeeded()
            pause = _FIRST_PAUSE
        except Exception as err:
            if working.stopping:
                return
            failures.failed(err)
            pause = min(pause * 2, _LAST_PAUSE)
        with contextlib.suppress(TimeoutError):
            await asyncio.wait_for(working.wake.wait(), pause)


async def _run_due[T](
    source: Source[T], handler: Callable[[T, Run], Awaitable[Outcome | None]], concurrency: int | None
) -> int:
    """Runs a stream that ends once nothing is due, and says how many jobs its handlers finished."""
    working = _Working()
    return await source.link.run(
        "read", lambda connection: _serve(connection, source, handler, concurrency, working, until_idle=True)
    )


async def _serve[T](
    connection: Connection,
    source: Source[T],
    handler: Callable[[T, Run], Awaitable[Outcome | None]],
    concurrency: int | None,
    working: _Working,
    *,
    until_idle: bool,
) -> int:
    """Opens a worker's stream on a connection and runs it to its end, which the store sends.

    After a stop or a GOAWAY once the jobs in hand are answered, or once
    nothing is due when it ends when idle. It says how many jobs its handlers
    finished.
    """
    handle = await handle_on(connection, source.open_method, source.opener)
    asked = JobsWork.encode(handle=handle, concurrency=concurrency, until_idle=until_idle or None)
    stream = await connection.session.open(methods["jobs.work"], asked, False)
    working.stream = stream
    if working.stopping:
        with contextlib.suppress(Exception):
            await stream.send(JobsAnswer.encode(how="stop"), False)
    serving = _WorkStream(source, handler, connection, stream)
    try:
        return await serving.serve()
    finally:
        if working.stream is stream:
            working.stream = None


class _WorkStream[T]:
    """A worker's stream on one connection: the jobs the store hands over, and the handlers they run here."""

    def __init__(
        self,
        source: Source[T],
        handler: Callable[[T, Run], Awaitable[Outcome | None]],
        connection: Connection,
        stream: Stream,
    ) -> None:
        self._source = source
        self._handler = handler
        self._connection = connection
        self._stream = stream
        self._runs: dict[int, tuple[asyncio.Task[None], list[bool]]] = {}
        """the runs under way, by number: each one's task, and whether a cancel took its job"""
        self._finished = 0

    async def serve(self) -> int:
        try:
            started = await self._stream.next()
            while not started.end:
                event = await self._stream.next()
                self._stream.consumed(len(event.body))
                if event.end:
                    break
                self._take(JobsHeld.decode(event.body))
            return self._finished
        except BaseException:
            for task, _ in self._runs.values():
                task.cancel()
            raise
        finally:
            running = [task for task, _ in self._runs.values()]
            if running:
                await asyncio.gather(*running, return_exceptions=True)

    def _take(self, held: dict[str, Any]) -> None:
        """A job the store handed over, run at once, since it sends no more than the handlers; or a cancel of one."""
        run = held.get("run", 0)
        if held.get("cancelled"):
            found = self._runs.get(run)
            if found is not None:
                found[1][0] = True
                found[0].cancel()
            return
        cancelled = [False]
        task = asyncio.ensure_future(self._run(held, cancelled))
        self._runs[run] = (task, cancelled)
        task.add_done_callback(lambda _: self._runs.pop(run, None))

    async def _run(self, held: dict[str, Any], cancelled: list[bool]) -> None:
        """Runs one job's handler and sends the answer it ended with; a job a cancel took gets none."""
        number = held.get("run", 0)

        async def report(progress: str) -> None:
            await self._stream.send(JobsAnswer.encode(run=number, how="progress", progress=progress), False)

        reporter = _Reporter(report)
        try:
            value = self._source.decode(held.get("value", "null"))
        except CorruptError as err:
            answer: dict[str, Any] = {"how": "fail", "error": f"its value no longer reads: {err}"}
        else:
            state = _RunState()
            run = Run(held, _Steps(self._connection, number), reporter, state)
            answer = await self._handled(value, run, state, cancelled)
        await reporter.stop()
        if cancelled[0]:
            return
        self._finished += 1
        with contextlib.suppress(Exception):
            await self._stream.send(JobsAnswer.encode({**answer, "run": number}), False)

    async def _handled(self, value: T, run: Run, state: _RunState, cancelled: list[bool]) -> dict[str, Any]:
        handling = asyncio.ensure_future(self._handler(value, run))
        try:
            returned = await asyncio.wait_for(asyncio.shield(handling), self._source.timeout)
        except TimeoutError:
            # past its timeout the handler is told to stop, and what it then
            # returns settles the job as ever
            state.stopped = True
            handling.cancel()
            try:
                returned = await handling
            except asyncio.CancelledError:
                return {"how": "retry", "error": f"the run passed its timeout of {self._source.timeout:g} s"}
            except Exception as err:
                return {"how": "retry", "error": message_of(err)}
        except asyncio.CancelledError:
            state.stopped = True
            handling.cancel()
            with contextlib.suppress(BaseException):
                await handling
            if cancelled[0]:
                return {"how": "retry", "error": "a cancel took the job while it ran"}
            raise
        except Exception as err:
            return {"how": "retry", "error": message_of(err)}
        return _answer_of(returned, state)
