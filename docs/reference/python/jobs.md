# Jobs API for Python

Every public class, function and type of the Jobs engine in `tinystore`, generated from its source. The [Jobs guide](../../guides/jobs/README.md) explains how to use them, and the [Bun and Node](../bun/jobs.md), [Go](../go/jobs.md) and [Rust](../rust/jobs.md) pages list the same API.

## Concurrency

```python
@dataclass(frozen=True, slots=True)
class Concurrency:
    total: int | None = None
    group: int | None = None  # at most this many jobs of one group at once: one customer's backlog holds back no other
```

How many jobs of a queue run at once: in all, across every worker of the store, and in each group.

## Backoff

```python
@dataclass(frozen=True, slots=True)
class Backoff:
    initial: Duration
    max: Duration
```

The wait before a retry: initial, doubling each time up to max, a tenth longer or shorter at random.

## LastRun

```python
@dataclass(frozen=True, slots=True)
class LastRun:
    started_at: datetime
    ended_at: datetime
```

When the last run a handler finished started and ended.

## Job

```python
@dataclass(frozen=True, slots=True)
class Job[T]:
    id: str | None
    value: T
    state: JobState
    at: datetime  # when it runs next; for one done or failed, when its last run was for
    attempt: int  # its attempts, a running one included
    ahead: int  # the jobs that run before a waiting one, up to 10,000
    progress: Any  # what its handler last reported of a running job
    error: str | None  # why its last run failed
    group: str | None
    repeat: str | None  # a repeating job's repeat as it keeps it: '@every 30s +6178ms', '10 3 * * * Europe/Berlin'
    last_run: LastRun | None
    expires_at: datetime | None = None  # when it leaves its queue unless written again: its ttl's end, or its expires_at
```

A job as its queue holds it.

## JobCounts

```python
@dataclass(frozen=True, slots=True)
class JobCounts:
    scheduled: int  # due later
    waiting: int  # due, and no handler runs it yet
    running: int
    failed: int  # kept for the queue's keep
```

How many jobs a queue holds in each state, from one snapshot; a done job has left it.

## JobPage

```python
@dataclass(frozen=True, slots=True)
class JobPage[T]:
    jobs: list[Job[T]]
    next: str | None
```

A page of a queue's jobs, and where the next starts when there is one.

## Outcome

```python
class Outcome:
    ...
```

How a run ends otherwise than done, made by run.retry, run.snooze or run.fail: its handler returns it.

## Run

```python
class Run:
    ...
```

This call on a job: what it is, its steps and progress, and the answers its handler returns.

A cancel that takes the job while it runs cancels its handler's task; so
does its timeout, after which ``stopped`` is True.

### Run.stopped

```python
@property
def stopped() -> bool: ...
```

Whether the run passed its timeout or a cancel took its job: the handler should stop too.

### Run.step

```python
async def step[R](name: str, fn: Callable[[], Awaitable[R] | R]) -> R: ...
```

Runs fn once in the job's run and keeps its answer, as JSON, across the job's attempts.

The attempt after a retry, a lost worker or a restart gets the answer
back without running fn again. A step whose attempt ends before its
answer is kept runs again, so what fn does outside the store should
bear doing twice. A name is the step's within its job, numbered in a
loop: "model:1", "tool:1".

### Run.set_progress

```python
def set_progress(progress: Any) -> None: ...
```

Shows how far the job got, any JSON within 4 KiB: the latest goes, ten a second at most.

### Run.retry

```python
def retry(when: Duration | datetime | None = None) -> Outcome: ...
```

Runs the job again after its backoff, or at the time given, the run counted as an attempt: return it.

### Run.snooze

```python
def snooze(when: Duration | datetime) -> Outcome: ...
```

Runs the job again at the time given, the run not counted: return it.

### Run.fail

```python
def fail(reason: str | BaseException) -> Outcome: ...
```

Fails the job for good now, its reason kept as its error: return it.

## Quiet

```python
@dataclass(frozen=True, slots=True)
class Quiet:
    from_: str
    to: str
    time_zone: str
```

Hours in which a job does not run: two wall-clock times of a day in a time zone.

``tinystore.Quiet(from_="23:00", to="07:00", time_zone="Europe/Moscow")``; a
``from_`` after ``to`` wraps midnight. A time the store gives a job inside
them, a retry's or a repeat's too, becomes their end. The same two, or a zone
no one has, is InvalidError.

## Gathered

```python
@dataclass(frozen=True, slots=True)
class Gathered:
    added: bool
    count: int
    at: datetime
```

What a gather did: whether it started the job, the items it holds now, and when it is due.

``count`` is 0 when a done job its dedupe holds keeps the id, and nothing was gathered.

## Dedupe

```python
@dataclass(frozen=True, slots=True)
class Dedupe:
    calendar: Calendar
    time_zone: str
```

Holds a done job's id until the local day, week or month it was done in ends, in its zone.

``dedupe=tinystore.Dedupe(calendar="day", time_zone="Europe/Moscow")``; a
span, ``dedupe="10m"``, holds it that long from when it was done instead.

## Queue

```python
class Queue[T]:
    name: str
```

A queue of jobs of one type, run in the order of their time: open it with ``store.queue(name, type)``.

### Queue.source

```python
@property
def source() -> Source[T]: ...
```

### Queue.add

```python
async def add(
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
) -> bool: ...
```

Adds a job unless its id is taken, and says whether it added one.

An id is taken by a job scheduled, waiting or running, or done while
the queue's dedupe keeps it; a job failed for good holds no id, and the
new job takes its place. It returns once the job is on disk; a job
without an id is always added.

### Queue.adding

```python
def adding(
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
) -> Adding: ...
```

A job's add for a batch of the database the queue was opened from.

``db.batch`` writes it with the batch's statements, so the job commits
with the rows or not at all, and shares the commit with every other
write of the file, where ``db.tx`` holds the writer and pays a sync of
its own. It adds as ``add`` does, unless its id is taken: its ``Done``
in the batch's answer changed 1 job, or 0.

    await db.batch([sql("update notes set body = ? where id = ?", body, id), indexing.adding(Index(id))])

### Queue.set

```python
async def set(
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
) -> None: ...
```

Makes the id's job this value at this time, whatever it was.

One waiting is made new, a failed one starts again, and a running one
runs once more after this run, with this value.

### Queue.update

```python
async def update(
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
) -> bool: ...
```

Changes the id's job when it is not running, its old value's steps dropped, and says whether it did.

### Queue.upsert

```python
async def upsert(
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
) -> bool: ...
```

Adds the job under id when it is free, or brings the one there to this call.

Its value, group, repeat and options of its own are written where they
differ, its time, attempts and steps kept. A changed value drops the
steps kept for the old one; a changed repeat moves a job not yet due to
the repeat's next time. It says whether it added or changed anything:
a job that already was so is not written, so it is cheap to call on
every request. ``at`` and ``delay`` count only when it adds.

    changed = await polls.upsert(f"group:{g}", Poll(group=g), every="3m")

### Queue.gather

```python
async def gather(
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
) -> Gathered: ...
```

Gathers items, a list, into the job under id, which runs settle after the last gather.

A job scheduled or waiting takes them after its own, in the order they
came, and is due settle after this gather, never later than max after
the gather that started it. With no job there it adds one of these
items; a running one runs on with what it read, and its next run takes
only what is gathered meanwhile, at its settle time, once it ends well; a
retry takes both, and a failure for good leaves the gathered ones a job of
their own. A done job its dedupe holds keeps the id, count 0; a failed one
gives it up, as to
add. A value that is no list, there or here, or a job that repeats, is
InvalidError:

    batch = store.queue("notify-batch", list[Notice])
    gathered = await batch.gather("grades:7", [Notice(course=1)], settle="5m", max="1h")

### Queue.cancel

```python
async def cancel(id: Job[T] | str) -> bool: ...
```

Takes a job, whatever its state, and says whether it was there.

It takes the job under an id, or a job a list gave: by its id, or for
one without, by the row it was listed from. A job retried, run again
for its repeat or cancelled since is in no row, and its cancel takes
nothing, never another job. A running one's handler task is cancelled,
and what it returns then settles nothing; a repeating one stops:

    async for job in emails.all(state="scheduled"):
        if job.value.to == bounced:
            await emails.cancel(job)

### Queue.counts

```python
async def counts() -> JobCounts: ...
```

How many jobs the queue holds in each state, from one snapshot; it reads no value.

### Queue.get

```python
async def get(id: str) -> Job[T] | None: ...
```

Where the job under id is; None for an id with no job.

### Queue.watch

```python
async def watch(id: str) -> AsyncIterator[Job[T]]: ...
```

The job under id as it is, then again each time it changes; it ends with the job.

Done, failed, or cancelled, a state only a watcher sees, end it. An id
with no job yields nothing. A watch whose connection is lost watches
again on the next; a job that ended meanwhile ends it without its last
state.

### Queue.list

```python
async def list(
    *,
    prefix: str | None = None,
    state: Literal['scheduled', 'waiting', 'running', 'failed'] | None = None,
    after: str | None = None,
    limit: int | None = None,
) -> JobPage[T]: ...
```

A page of the queue's jobs: with no prefix every job, those with ids by id,
then those without as they run, then those without that failed.

### Queue.all

```python
def all(
    *,
    prefix: str | None = None,
    state: Literal['scheduled', 'waiting', 'running', 'failed'] | None = None,
    limit: int | None = None,
) -> AsyncIterator[Job[T]]: ...
```

Every job the options name, a page at a time, holding nothing between pages.

A job whose value no longer reads is a CorruptError in its place,
naming it, and iterating the same walk again goes on past it.

### Queue.work

```python
def work(
    handler: Callable[[T, Run], Awaitable[Outcome | None]],
    *,
    concurrency: int | None = None,
) -> Worker: ...
```

Runs handler on each job as it falls due, and returns the running worker at once.

As many run at once as the queue's concurrency lets, one when it sets
none, or concurrency under it. Stop it at shutdown, as store.close()
does. A worker whose connection is lost reaches the store again, the
jobs it held running again as a dead worker's would.

### Queue.run_due

```python
async def run_due(
    handler: Callable[[T, Run], Awaitable[Outcome | None]],
    *,
    concurrency: int | None = None,
) -> int: ...
```

Runs what is due when it starts and what falls due meanwhile, then says how many jobs ran.

It never waits for a later job. Tests, scripts and commands use it.

## Adding

```python
@dataclass(frozen=True, slots=True)
class Adding:
    queue: str  # the queue's name
    source: Source[Any] = field(repr=False)
    call: dict[str, Any] = field(repr=False)  # the add's call but its handle
```

A job's add that a database's batch carries, from ``queue.adding``: it commits with the batch's statements.

## StoreTxQueue

```python
class StoreTxQueue[T]:
    ...
```

A queue's writes inside the store's transaction, from ``tx.with_(queue)`` in ``store.tx``.

Each is kept for the commit, which makes them all or none, so what one
did is known once the transaction has committed.

### StoreTxQueue.add

```python
async def add(
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
) -> None: ...
```

Adds a job unless its id is taken, with the commit.

### StoreTxQueue.set

```python
async def set(
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
) -> None: ...
```

Makes the id's job this value at this time, whatever it was, with the commit.

### StoreTxQueue.update

```python
async def update(
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
) -> None: ...
```

Changes the id's job when it is not running, its old value's steps dropped, with the commit.

### StoreTxQueue.upsert

```python
async def upsert(
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
) -> None: ...
```

Adds the job under id, or brings the one there to this call, its time kept, with the commit.

### StoreTxQueue.cancel

```python
async def cancel(id: Job[T] | str) -> None: ...
```

Takes the job under id, or a job a list gave, whatever its state, with the commit.

## Schedule

```python
class Schedule:
    name: str
```

A repeat the code owns: one repeating job under its name, and the worker that runs it.

### Schedule.get

```python
async def get() -> Job[None] | None: ...
```

When it runs next and how its last run went: what a page of cron jobs shows.

### Schedule.run_due

```python
async def run_due() -> int: ...
```

Runs the schedule's job when it is due, as a queue's run_due does.

### Schedule.stop

```python
async def stop() -> None: ...
```

Stops the schedule's worker, as a worker's stop does.

### Schedule.cancel

```python
async def cancel() -> bool: ...
```

Removes the schedule's job, until the program opens it again.

### Schedule.run_now

```python
async def run_now() -> None: ...
```

Runs the schedule once as soon as a worker is free, its later times as they were.

Run at 03:29, a schedule of 03:30 runs again at 03:30. A run under way
is followed by one more right after it, never beside it. A schedule
whose job a cancel took is ``NotFoundError`` until the program opens it
again.

## Worker

```python
class Worker:
    ...
```

A worker running a queue's handlers here while the store's loop claims their jobs.

### Worker.stop

```python
async def stop() -> None: ...
```

Takes no job more and waits for the handlers under way, each bounded by its timeout.

The store gives back the jobs it held for them, and ends the worker
once their answers are written.

<!-- Generated by task reference from sdk/python/src/tinystore/jobs.py. Edit the doc comments there, not this file. -->
