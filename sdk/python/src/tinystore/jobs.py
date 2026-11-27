"""Work an application must do later, or now but outside the request that asked for it.

As docs/guides/jobs/README.md has it: queues of JSON values run in the order of their
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
from ._time import Calendar, Duration, moment, ms, unix_ms
from ._values import from_json, to_json
from ._wire.protocol import (
    Handle,
    JobsAnswer,
    JobsCall,
    JobsChanged,
    JobsCounts,
    JobsGather,
    JobsGathered,
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
from .errors import CorruptError, InvalidError, TinystoreError, UnavailableError, error_of
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
    """the jobs that run before a waiting one, exactly up to 10,000"""
    progress: Any
    """what its handler last reported of a running job"""
    error: str | None
    """why its last run failed"""
    group: str | None
    repeat: str | None
    """a repeating job's repeat as it keeps it: '@every 30s +6178ms', '10 3 * * * Europe/Berlin'"""
    last_run: LastRun | None
    expires_at: datetime | None = None
    """when it leaves its queue unless written again: its ttl's end, or its expires_at"""
    ahead_at_least: bool = False
    """counting ``ahead`` stopped at 10,000, more being there: past it ``ahead`` is 10,000, never an estimate"""
    _row: str | None = field(default=None, repr=False, compare=False)
    """where a job without an id lies, which a cancel names it by"""


@dataclass(frozen=True, slots=True)
class JobCounts:
    """How many jobs a queue holds in each state, from one snapshot; a done job has left it."""

    scheduled: int
    """due later"""
    waiting: int
    """due, and no handler runs it yet"""
    running: int
    failed: int
    """kept for the queue's keep"""


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
        named = "a job without an id" if found.get("id") is None else f"id {found['id']!r}"
        raise CorruptError(f"{source.describe}: {named}: {err}") from None
    started = found.get("started_at")
    progress = found.get("progress")
    return Job(
        id=found.get("id"),
        value=value,
        state=found.get("state", "waiting"),
        at=moment(found.get("at", 0)),
        attempt=found.get("attempt", 0),
        ahead=found.get("ahead", 0),
        ahead_at_least=found.get("ahead_at_least", False),
        progress=None if progress is None else json.loads(progress),
        error=found.get("error"),
        group=found.get("group"),
        repeat=found.get("repeat"),
        last_run=None if started is None else LastRun(moment(started), moment(found.get("ended_at", started))),
        expires_at=None if found.get("expires_at") is None else moment(found["expires_at"]),
        _row=found.get("row"),
    )


async def _get[T](source: Source[T], job_id: str) -> Job[T] | None:
    found = JobsJob.decode(await source.call("jobs.get", lambda h: JobsId.encode(handle=h, id=job_id), "read"))
    return _job_of(source, found) if found.get("found") else None


def _named_by(job: Job[Any] | str, describe: str) -> dict[str, str]:
    """What a cancel sends of a job: its id, or for one without, the row a page gave."""
    if isinstance(job, str):
        return {"id": job}
    if job.id is not None:
        return {"id": job.id}
    row = job._row  # pyright: ignore[reportPrivateUsage]
    if row is None:
        raise InvalidError(f"{describe}: a job without an id names no row: cancel the job a list gave")
    return {"row": row}


async def _cancel(source: Source[Any], job: Job[Any] | str) -> bool:
    named = _named_by(job, source.describe)
    body = await source.call("jobs.cancel", lambda h: JobsId.encode(handle=h, **named), "write")
    return JobsChanged.decode(body).get("changed", False)


async def _page[T](
    source: Source[T], fields: dict[str, Any], walk: bool
) -> tuple[list[Job[T] | TinystoreError], str | None]:
    """A page of the queue's jobs; a walk's has a job whose value no longer reads as its error in its place."""
    body = await source.call("jobs.list", lambda h: JobsList.encode(handle=h, walk=walk, **fields), "read")
    page = JobsPage.decode(body)
    jobs: list[Job[T] | TinystoreError] = []
    for found in page.get("jobs", []):
        failure = found.get("failure")
        if failure is not None:
            jobs.append(error_of(failure.get("code", "internal"), failure.get("message", ""), failure.get("what")))
            continue
        try:
            jobs.append(_job_of(source, found))
        except CorruptError as err:
            if not walk:
                raise
            jobs.append(err)
    return jobs, page.get("next")


class _Walk[T]:
    """Every job a filter names, a page at a time.

    A job whose value no longer reads is its error in its place: the next
    step raises it, and the step after goes on past it, as an ``async for``
    that catches it and iterates the same walk again does.
    """

    def __init__(self, source: Source[T], fields: dict[str, Any]) -> None:
        self._source = source
        self._fields = fields
        self._jobs: list[Job[T] | TinystoreError] = []
        self._after: str | None = None
        self._ended = False

    def __aiter__(self) -> Self:
        return self

    async def __anext__(self) -> Job[T]:
        while not self._jobs and not self._ended:
            self._jobs, self._after = await _page(self._source, {**self._fields, "after": self._after}, True)
            self._ended = self._after is None
        if not self._jobs:
            raise StopAsyncIteration
        job = self._jobs.pop(0)
        if isinstance(job, TinystoreError):
            raise job
        return job


def _shows_the_same(a: dict[str, Any], b: dict[str, Any]) -> bool:
    """Whether two reports show a watcher the same: the job's state, place, attempt, time, progress and error."""
    return all(a.get(k) == b.get(k) for k in ("state", "ahead", "ahead_at_least", "attempt", "at", "progress", "error"))


def job_fields(
    *,
    at: datetime | None = None,
    delay: Duration | None = None,
    group: str | None = None,
    every: Duration | None = None,
    cron: str | None = None,
    time_zone: str | None = None,
    dedupe: Duration | Dedupe | None = None,
    keep: Duration | None = None,
    ttl: Duration | None = None,
    expires_at: datetime | None = None,
    quiet: Quiet | Literal[False] | None = None,
) -> dict[str, Any]:
    """A job's time, group, repeat and options of its own, as an add, a set and an update take them."""
    return {
        "at": None if at is None else unix_ms(at, "a job's time"),
        "delay": None if delay is None else ms(delay, "a job's delay"),
        "group": group,
        "every": None if every is None else ms(every, "a job's repeat"),
        "cron": cron,
        "time_zone": time_zone,
        **dedupe_fields(dedupe, "a job's dedupe"),
        "keep": None if keep is None else ms(keep, "a job's keep"),
        "ttl": None if ttl is None else ms(ttl, "a job's ttl"),
        "expires_at": None if expires_at is None else unix_ms(expires_at, "a job's term"),
        "quiet": quiet_fields(quiet) if quiet else None,
        "no_quiet": quiet is False,
    }


@dataclass(frozen=True, slots=True)
class Quiet:
    """Hours in which a job does not run: two wall-clock times of a day in a time zone.

    ``tinystore.Quiet(from_="23:00", to="07:00", time_zone="Europe/Moscow")``; a
    ``from_`` after ``to`` wraps midnight. A time the store gives a job inside
    them, a retry's or a repeat's too, becomes their end. The same two, or a zone
    no one has, is InvalidError.
    """

    from_: str
    to: str
    time_zone: str


def quiet_fields(quiet: Quiet | None) -> dict[str, str] | None:
    """Quiet hours as a queue's open and a job's call carry them."""
    if quiet is not None and not isinstance(quiet, Quiet):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"quiet={quiet!r}: quiet hours are a tinystore.Quiet, or False on a call for none")
    return None if quiet is None else {"from": quiet.from_, "to": quiet.to, "time_zone": quiet.time_zone}


@dataclass(frozen=True, slots=True)
class Gathered:
    """What a gather did: whether it started the job, the items it holds now, and when it is due.

    ``count`` is 0 when a done job its dedupe holds keeps the id, and nothing was gathered.
    """

    added: bool
    count: int
    at: datetime


@dataclass(frozen=True, slots=True)
class Dedupe:
    """Holds a done job's id until the local day, week or month it was done in ends, in its zone.

    ``dedupe=tinystore.Dedupe(calendar="day", time_zone="Europe/Moscow")``; a
    span, ``dedupe="10m"``, holds it that long from when it was done instead.
    """

    calendar: Calendar
    time_zone: str


def dedupe_fields(dedupe: Duration | Dedupe | None, what: str) -> dict[str, Any]:
    """A dedupe as a queue's open and a job's call carry it: a span, or a calendar in its place."""
    if isinstance(dedupe, Dedupe):
        return {"dedupe": None, "dedupe_calendar": {"calendar": dedupe.calendar, "time_zone": dedupe.time_zone}}
    return {"dedupe": None if dedupe is None else ms(dedupe, what), "dedupe_calendar": None}


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
        dedupe: Duration | Dedupe | None = None,
        keep: Duration | None = None,
        ttl: Duration | None = None,
        expires_at: datetime | None = None,
        quiet: Quiet | Literal[False] | None = None,
    ) -> bool:
        """Adds a job unless its id is taken, and says whether it added one.

        An id is taken by a job scheduled, waiting or running, or done while
        the queue's dedupe keeps it; a job failed for good holds no id, and the
        new job takes its place. It returns once the job is on disk; a job
        without an id is always added.
        """
        timing = job_fields(
            at=at,
            delay=delay,
            group=group,
            every=every,
            cron=cron,
            time_zone=time_zone,
            dedupe=dedupe,
            keep=keep,
            ttl=ttl,
            expires_at=expires_at,
            quiet=quiet,
        )
        fields = {**timing, "id": id, "value": self._source.encode(value)}
        return await self._changed("jobs.add", fields)

    def adding(
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
        dedupe: Duration | Dedupe | None = None,
        keep: Duration | None = None,
        ttl: Duration | None = None,
        expires_at: datetime | None = None,
        quiet: Quiet | Literal[False] | None = None,
    ) -> Adding:
        """A job's add for a batch of the database the queue was opened from.

        ``db.batch`` writes it with the batch's statements, so the job commits
        with the rows or not at all, and shares the commit with every other
        write of the file, where ``db.tx`` holds the writer and pays a sync of
        its own. It adds as ``add`` does, unless its id is taken: its ``Done``
        in the batch's answer changed 1 job, or 0.

            await db.batch([sql("update notes set body = ? where id = ?", body, id), indexing.adding(Index(id))])
        """
        timing = job_fields(
            at=at,
            delay=delay,
            group=group,
            every=every,
            cron=cron,
            time_zone=time_zone,
            dedupe=dedupe,
            keep=keep,
            ttl=ttl,
            expires_at=expires_at,
            quiet=quiet,
        )
        return Adding(self.name, self._source, {**timing, "id": id, "value": self._source.encode(value)})

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
        dedupe: Duration | Dedupe | None = None,
        keep: Duration | None = None,
        ttl: Duration | None = None,
        expires_at: datetime | None = None,
        quiet: Quiet | Literal[False] | None = None,
    ) -> None:
        """Makes the id's job this value at this time, whatever it was.

        One waiting is made new, a failed one starts again, and a running one
        runs once more after this run, with this value.
        """
        timing = job_fields(
            at=at,
            delay=delay,
            group=group,
            every=every,
            cron=cron,
            time_zone=time_zone,
            dedupe=dedupe,
            keep=keep,
            ttl=ttl,
            expires_at=expires_at,
            quiet=quiet,
        )
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
        dedupe: Duration | Dedupe | None = None,
        keep: Duration | None = None,
        ttl: Duration | None = None,
        expires_at: datetime | None = None,
        quiet: Quiet | Literal[False] | None = None,
    ) -> bool:
        """Changes the id's job when it is not running, its old value's steps dropped, and says whether it did."""
        timing = job_fields(
            at=at,
            delay=delay,
            group=group,
            every=every,
            cron=cron,
            time_zone=time_zone,
            dedupe=dedupe,
            keep=keep,
            ttl=ttl,
            expires_at=expires_at,
            quiet=quiet,
        )
        fields = {**timing, "id": id, "value": self._source.encode(value)}
        return await self._changed("jobs.update", fields)

    async def upsert(
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
        dedupe: Duration | Dedupe | None = None,
        keep: Duration | None = None,
        ttl: Duration | None = None,
        expires_at: datetime | None = None,
        quiet: Quiet | Literal[False] | None = None,
    ) -> bool:
        """Adds the job under id when it is free, or brings the one there to this call.

        Its value, group, repeat and options of its own are written where they
        differ, its time, attempts and steps kept. A changed value drops the
        steps kept for the old one; a changed repeat moves a job not yet due to
        the repeat's next time. It says whether it added or changed anything:
        a job that already was so is not written, so it is cheap to call on
        every request. ``at`` and ``delay`` count only when it adds.

            changed = await polls.upsert(f"group:{g}", Poll(group=g), every="3m")
        """
        timing = job_fields(
            at=at,
            delay=delay,
            group=group,
            every=every,
            cron=cron,
            time_zone=time_zone,
            dedupe=dedupe,
            keep=keep,
            ttl=ttl,
            expires_at=expires_at,
            quiet=quiet,
        )
        fields = {**timing, "id": id, "value": self._source.encode(value)}
        return await self._changed("jobs.upsert", fields)

    async def gather(
        self,
        id: str,
        items: T,
        *,
        settle: Duration,
        max: Duration | None = None,
        group: str | None = None,
        dedupe: Duration | Dedupe | None = None,
        keep: Duration | None = None,
        ttl: Duration | None = None,
        expires_at: datetime | None = None,
        quiet: Quiet | Literal[False] | None = None,
    ) -> Gathered:
        """Gathers items, a list, into the job under id, which runs settle after the last gather.

        A job scheduled or waiting takes them after its own, in the order they
        came, and is due settle after this gather, never later than max after
        the gather that started it. With no job there it adds one of these
        items; a running one runs on with what it read, and its next run takes
        only what is gathered meanwhile, at its settle time, once it ends well; a
        retry takes both, and a failure for good leaves the gathered ones a job of
        their own. A done job its dedupe holds keeps the id, count 0; a failed one
        gives it up, as to
        add. A value that is no list, there or here, or a job that repeats, is
        InvalidError::

            batch = store.queue("notify-batch", list[Notice])
            gathered = await batch.gather("grades:7", [Notice(course=1)], settle="5m", max="1h")
        """
        own = job_fields(group=group, dedupe=dedupe, keep=keep, ttl=ttl, expires_at=expires_at, quiet=quiet)
        call = {**own, "id": id, "value": self._source.encode(items)}
        asked = {"settle": ms(settle, "a gather's settle"), "max": None if max is None else ms(max, "a gather's max")}
        body = await self._source.call(
            "jobs.gather", lambda h: JobsGather.encode(asked, call={**call, "handle": h}), "write"
        )
        gathered = JobsGathered.decode(body)
        return Gathered(
            added=gathered.get("added", False), count=gathered.get("count", 0), at=moment(gathered.get("at", 0))
        )

    async def cancel(self, id: Job[T] | str) -> bool:
        """Takes a job, whatever its state, and says whether it was there.

        It takes the job under an id, or a job a list gave: by its id, or for
        one without, by the row it was listed from. A job retried, run again
        for its repeat or cancelled since is in no row, and its cancel takes
        nothing, never another job. A running one's handler task is cancelled,
        and what it returns then settles nothing; a repeating one stops::

            async for job in emails.all(state="scheduled"):
                if job.value.to == bounced:
                    await emails.cancel(job)
        """
        return await _cancel(self._source, id)

    async def counts(self) -> JobCounts:
        """How many jobs the queue holds in each state, from one snapshot; it reads no value."""
        body = await self._source.call("jobs.counts", lambda h: Handle.encode(handle=h), "read")
        counts = JobsCounts.decode(body)
        return JobCounts(
            scheduled=counts.get("scheduled", 0),
            waiting=counts.get("waiting", 0),
            running=counts.get("running", 0),
            failed=counts.get("failed", 0),
        )

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
            unfollow = source.link.follow(stream)
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
                unfollow()

    async def list(
        self,
        *,
        prefix: str | None = None,
        state: Literal["scheduled", "waiting", "running", "failed"] | None = None,
        after: str | None = None,
        limit: int | None = None,
    ) -> JobPage[T]:
        """A page of the queue's jobs: with no prefix every job, those with ids by id,
        then those without as they run, then those without that failed."""
        fields = {"prefix": prefix, "state": state, "after": after, "limit": limit}
        jobs, next_page = await _page(self._source, fields, False)
        return JobPage(cast("list[Job[T]]", jobs), next_page)

    def all(
        self,
        *,
        prefix: str | None = None,
        state: Literal["scheduled", "waiting", "running", "failed"] | None = None,
        limit: int | None = None,
    ) -> AsyncIterator[Job[T]]:
        """Every job the options name, a page at a time, holding nothing between pages.

        A job whose value no longer reads is a CorruptError in its place,
        naming it, and iterating the same walk again goes on past it.
        """
        return _Walk(self._source, {"prefix": prefix, "state": state, "limit": limit})

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


@dataclass(frozen=True, slots=True)
class Adding:
    """A job's add that a database's batch carries, from ``queue.adding``: it commits with the batch's statements."""

    queue: str
    """the queue's name"""
    source: Source[Any] = field(repr=False)
    call: dict[str, Any] = field(repr=False)
    """the add's call but its handle"""


def adding_in(adding: Adding, home: Home) -> Callable[[Connection], Awaitable[dict[str, Any]]]:
    """What a batch of home sends of an add, on the connection it goes on: its call, its queue's handle opened there.

    A queue kept outside home is refused, since its job would not commit
    with the rows.
    """
    source, call = adding.source, adding.call
    if source.home is not home:
        kept = "jobs.db" if source.home is None else source.home.label
        raise InvalidError(
            f"{home.label}: jobs queue {adding.queue}: kept in {kept}, outside this batch of {home.label}"
        )

    async def sent(connection: Connection) -> dict[str, Any]:
        handle = await handle_on(connection, source.open_method, source.opener)
        return {"text": "", "values": [], "method": methods["jobs.add"], "body": JobsCall.encode(call, handle=handle)}

    return sent


@dataclass(slots=True)
class JobOp:
    """A write the store's transaction keeps for its commit: its method, and its call or the job it cancels."""

    source: Source[Any]
    method: str
    call: dict[str, Any] | None = None
    id: dict[str, str] | None = None


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
        dedupe: Duration | Dedupe | None = None,
        keep: Duration | None = None,
        ttl: Duration | None = None,
        expires_at: datetime | None = None,
        quiet: Quiet | Literal[False] | None = None,
    ) -> None:
        """Adds a job unless its id is taken, with the commit."""
        timing = job_fields(
            at=at,
            delay=delay,
            group=group,
            every=every,
            cron=cron,
            time_zone=time_zone,
            dedupe=dedupe,
            keep=keep,
            ttl=ttl,
            expires_at=expires_at,
            quiet=quiet,
        )
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
        dedupe: Duration | Dedupe | None = None,
        keep: Duration | None = None,
        ttl: Duration | None = None,
        expires_at: datetime | None = None,
        quiet: Quiet | Literal[False] | None = None,
    ) -> None:
        """Makes the id's job this value at this time, whatever it was, with the commit."""
        timing = job_fields(
            at=at,
            delay=delay,
            group=group,
            every=every,
            cron=cron,
            time_zone=time_zone,
            dedupe=dedupe,
            keep=keep,
            ttl=ttl,
            expires_at=expires_at,
            quiet=quiet,
        )
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
        dedupe: Duration | Dedupe | None = None,
        keep: Duration | None = None,
        ttl: Duration | None = None,
        expires_at: datetime | None = None,
        quiet: Quiet | Literal[False] | None = None,
    ) -> None:
        """Changes the id's job when it is not running, its old value's steps dropped, with the commit."""
        timing = job_fields(
            at=at,
            delay=delay,
            group=group,
            every=every,
            cron=cron,
            time_zone=time_zone,
            dedupe=dedupe,
            keep=keep,
            ttl=ttl,
            expires_at=expires_at,
            quiet=quiet,
        )
        self._write("jobs.update", {**timing, "id": id}, value)

    async def upsert(
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
        dedupe: Duration | Dedupe | None = None,
        keep: Duration | None = None,
        ttl: Duration | None = None,
        expires_at: datetime | None = None,
        quiet: Quiet | Literal[False] | None = None,
    ) -> None:
        """Adds the job under id, or brings the one there to this call, its time kept, with the commit."""
        timing = job_fields(
            at=at,
            delay=delay,
            group=group,
            every=every,
            cron=cron,
            time_zone=time_zone,
            dedupe=dedupe,
            keep=keep,
            ttl=ttl,
            expires_at=expires_at,
            quiet=quiet,
        )
        self._write("jobs.upsert", {**timing, "id": id}, value)

    async def cancel(self, id: Job[T] | str) -> None:
        """Takes the job under id, or a job a list gave, whatever its state, with the commit."""
        self._keep(JobOp(self._source, "jobs.cancel", id=_named_by(id, self._source.describe)))

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
                    "id": None if op.id is None else {"handle": handle, **op.id},
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

    async def run_now(self) -> None:
        """Runs the schedule once as soon as a worker is free, its later times as they were.

        Run at 03:29, a schedule of 03:30 runs again at 03:30. A run under way
        is followed by one more right after it, never beside it. A schedule
        whose job a cancel took is ``NotFoundError`` until the program opens it
        again.
        """
        await self._source.call("jobs.runNow", lambda h: Handle.encode(handle=h), "write")


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
    dedupe: Duration | Dedupe | None = None
    keep: Duration | None = None
    max_waiting: int | None = None
    group_rate: str | None = None
    """jobs of each group started in any span, beside rate: "2/s", each account's own limit"""
    quiet: Quiet | None = None
    """hours in which the queue's jobs do not run, which a job's own win over"""


def queue_fields(name: str, describe: str, options: QueueOptions) -> dict[str, Any]:
    """A queue's open, checked before anything leaves."""
    check_name(name, "queue")
    concurrency = options.concurrency
    if isinstance(concurrency, int):
        concurrency = Concurrency(total=concurrency)
    rate = None if options.rate is None else dict(zip(("count", "per"), rate_of(options.rate), strict=True))
    group_rate = (
        None if options.group_rate is None else dict(zip(("count", "per"), rate_of(options.group_rate), strict=True))
    )
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
        "group_rate": group_rate,
        **dedupe_fields(options.dedupe, f"{describe}: dedupe"),
        "keep": None if options.keep is None else ms(options.keep, f"{describe}: keep"),
        "max_waiting": _whole(options.max_waiting, describe, "a max_waiting"),
        "quiet": quiet_fields(options.quiet),
    }


def open_queue(
    link: Link, workers: set[Worker], name: str, of: Any, options: QueueOptions, home: Home | None = None
) -> Queue[Any]:
    """A queue, its open checked before anything leaves.

    home is the database whose file keeps it, its open naming the database's
    handle on each connection; the store's jobs.db when None.
    """
    describe = f"jobs queue {name}" if home is None else f"{home.label}: jobs queue {name}"
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
