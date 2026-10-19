# Jobs API for Python

Every public class, function and type of the Jobs engine in `tinystore`, generated from its source. The [Jobs guide](../../chat/replies.md) explains how to use them, and the [Bun and Node](../bun/jobs.md), [Go](../go/jobs.md) and [Rust](../rust/jobs.md) pages list the same API.

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
```

A job as its queue holds it.

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
) -> bool: ...
```

Adds a job unless its id is taken, and says whether it added one.

An id is taken by a job scheduled, waiting, running or failed, or done
while the queue's dedupe keeps it. It returns once the job is on disk;
a job without an id is always added.

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
) -> bool: ...
```

Changes the id's job when it has not started, and says whether it did.

### Queue.cancel

```python
async def cancel(id: str) -> bool: ...
```

Takes the job under id, whatever its state, and says whether there was one.

A running one's handler task is cancelled, and what it returns then
settles nothing; a repeating one stops.

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

A page of the queue's jobs whose ids start with a prefix, or of its failed ones, the last first.

### Queue.all

```python
async def all(
    *,
    prefix: str | None = None,
    state: Literal['scheduled', 'waiting', 'running', 'failed'] | None = None,
    limit: int | None = None,
) -> AsyncIterator[Job[T]]: ...
```

Every job the options name, a page at a time, holding nothing between pages.

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
) -> None: ...
```

Changes the id's job when it has not started, with the commit.

### StoreTxQueue.cancel

```python
async def cancel(id: str) -> None: ...
```

Takes the job under id, whatever its state, with the commit.

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
