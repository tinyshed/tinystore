# Jobs API for Python

Every public class, function and type of the Jobs engine in `tinystore`, generated from its source. The [Jobs guide](../../jobs/README.md) explains how to use them, and the [Bun and Node](../bun/jobs.md) and [Go](../go/jobs.md) pages list the same API.

## Repeat

```python
@dataclass(frozen=True, slots=True)
class Repeat:
    cron: str | None = None
    zone: str | None = None
    every: int | None = None
    spread: bool = False
```

When a job runs again: cron text in a zone by its name, or every so many milliseconds, spread or not.

### Repeat.fields

```python
def fields() -> dict[str, Any]: ...
```

## cron

```python
def cron(expression: str, zone: str) -> Repeat: ...
```

## daily

```python
def daily(at: str, zone: str) -> Repeat: ...
```

Every day at a time of the zone's clock: daily("03:10", "Europe/Moscow").

## every

```python
def every(d: Duration, *, spread: bool = False) -> Repeat: ...
```

Every so often; spread runs each key's job at a phase of its own: every("30s", spread=True).

## Enqueue

```python
@dataclass(frozen=True, slots=True)
class Enqueue[V]:
    value: V
    at: datetime | None = None
    after: Duration | None = None
    key: str | None = None
    repeat: Repeat | None = None
    move: bool = False
    group: str | None = None
```

A job for enqueue_all.

## JobEntry

```python
@dataclass(frozen=True, slots=True)
class JobEntry[V]:
    key: str
    value: V
    at: datetime | None
    attempt: int
    state: str
    error: str | None
    repeat: str | None
    ahead: int = 0  # the jobs that run before a waiting one, up to 10,000; get and watch count it, scan leaves it 0
    progress: Any = None  # what a running job's handler last reported with job.progress
    ran: datetime | None = None  # when the last run a handler finished began: acknowledged, retried, failed or snoozed
    took: float | None = None  # how many seconds that run took
```

A job as its queue holds it; state is waiting, running, failed, done or cancelled.

## Job

```python
@dataclass
class Job[V]:
    key: str
    value: V
    at: datetime | None
    attempt: int
    settlement: dict[str, Any] | None = field(default=None, repr=False)
```

A job in a work loop's handler; retry, fail or snooze decide how it settles instead of its return.

A cancel that takes the job while it runs cancels the handler's task.

### Job.step

```python
async def step[R](name: str, fn: Callable[[], Awaitable[R] | R]) -> R: ...
```

Runs fn once in the job's run and keeps what it answers, as JSON.

An attempt after a retry, a lost lease or a restart gets the kept
answer back without running fn again. A step whose attempt ends before
its answer is kept runs again, so what fn does outside the store should
bear doing twice. A name is the step's within the run, which a loop
numbers: "model:1", "tool:1", "model:2". An answer is at most 1 MiB of
JSON.

    hits = await job.step("search", lambda: search(job.value["query"]))

### Job.progress

```python
def progress(progress: object) -> None: ...
```

Reports how far the job got, any JSON within 4 KiB, which get and watch show until it is settled.

It does not wait for the server.

### Job.retry

```python
def retry(
    error: object = None,
    *,
    at: datetime | None = None,
    after: Duration | None = None,
) -> None: ...
```

### Job.fail

```python
def fail(error: object = None) -> None: ...
```

### Job.snooze

```python
def snooze(*, at: datetime | None = None, after: Duration | None = None) -> None: ...
```

Moves the job to another time without counting the attempt.

## ClaimedJob

```python
class ClaimedJob[V]:
    ...
```

A job a claim leased, which its caller settles before the lease ends, on the claim's connection.

### ClaimedJob.step

```python
async def step[R](name: str, fn: Callable[[], Awaitable[R] | R]) -> R: ...
```

Runs fn once in the job's run and keeps its answer, as a work loop's job.step does.

### ClaimedJob.ack

```python
async def ack() -> None: ...
```

### ClaimedJob.retry

```python
async def retry(
    error: object = None,
    *,
    at: datetime | None = None,
    after: Duration | None = None,
) -> None: ...
```

### ClaimedJob.fail

```python
async def fail(error: object = None) -> None: ...
```

### ClaimedJob.snooze

```python
async def snooze(
    *,
    at: datetime | None = None,
    after: Duration | None = None,
) -> None: ...
```

### ClaimedJob.extend

```python
async def extend(d: Duration) -> None: ...
```

Holds the job this much longer from now.

### ClaimedJob.progress

```python
async def progress(progress: object) -> None: ...
```

Reports how far the job got, any JSON within 4 KiB, which get and watch show until it is settled.

## Jobs

```python
class Jobs:
    ...
```

### Jobs.queue

```python
def queue[V](
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
    max_running_in_group: int | None = None,
    rate: str | None = None,
    in_: Database | None = None,
) -> Queue[V]: ...
```

A queue of JSON values of one type; its policy is the options' and the server's defaults.

max_running bounds the jobs that run at once across every worker of
the store, max_running_in_group those of one group, an enqueue's
group, and rate how many start in any span: "30/s". in_ keeps the
queue in a database's file instead of jobs.db, so that a batch of the
database commits a job with its rows:

    index = store.jobs.queue("index", IndexNote, in_=db)
    async with db.batch() as tx:
        tx.exec("update notes set body = ? where id = ?", body, note_id)
        index.with_tx(tx).enqueue(IndexNote(note_id))

### Jobs.schedule

```python
def schedule(
    name: str,
    repeat: Repeat,
    /,
    in_: Database | None = None,
    **options: Any,
) -> Queue[None]: ...
```

A queue of one job under the schedule's name, repeating as it says; work runs it, cancel stops it.

## Queue

```python
class Queue[V]:
    ...
```

### Queue.with_tx

```python
def with_tx(tx: SqlBatch) -> QueueTx[V]: ...
```

The queue's enqueue inside a batch of the database it lives in, opened in_: the job commits with its rows.

### Queue.enqueue

```python
async def enqueue(
    value: V,
    *,
    at: datetime | None = None,
    after: Duration | None = None,
    key: str | None = None,
    repeat: Repeat | None = None,
    move: bool = False,
    group: str | None = None,
) -> None: ...
```

Adds a job; it returns once the job is in the file.

move sets the time of the key's job either way, later too, and a
running one's next run; group names the group whose running jobs the
queue's max_running_in_group bounds.

### Queue.enqueue_all

```python
async def enqueue_all(jobs: Iterable[Enqueue[V]]) -> None: ...
```

Adds jobs in one transaction, all or none; a refused one names itself as call.

### Queue.update

```python
async def update(
    key: str,
    value: V,
    *,
    at: datetime | None = None,
    after: Duration | None = None,
    repeat: Repeat | None = None,
    group: str | None = None,
) -> None: ...
```

Changes a job that waits or failed; one a worker holds, done or absent is ConflictError.

### Queue.cancel

```python
async def cancel(key: str) -> bool: ...
```

Removes the job under key, whether it waits, runs or failed, and says whether there was one.

A running job's handler task is cancelled, and what it returns settles nothing.

### Queue.get

```python
async def get(key: str) -> JobEntry[V] | None: ...
```

The job a key names, waiting, running or failed, or done while keep_done keeps its key.

### Queue.watch

```python
async def watch(key: str) -> AsyncIterator[JobEntry[V]]: ...
```

Yields the job under key as it is, then at each change, until it ends: done, failed or cancelled.

A change is of its state, place, attempt, time, progress or error, and
the last entry is the one that ended it. A key that names no job yields
nothing. Leaving the loop ends the watch; a connection lost is
connected again, and the watch goes on from the job as it is then.

    async for s in videos.watch(video_id):
        await send(s.state, s.ahead, s.progress)

### Queue.scan

```python
async def scan(
    *,
    prefix: str | None = None,
    state: Literal['failed'] | None = None,
    after: str | None = None,
    limit: int | None = None,
) -> Page[JobEntry[V], str]: ...
```

A page of the jobs under a prefix in the byte order of their keys, and where the next begins.

### Queue.all

```python
async def all(
    *,
    prefix: str | None = None,
    state: Literal['failed'] | None = None,
    limit: int | None = None,
) -> AsyncIterator[JobEntry[V]]: ...
```

### Queue.claim

```python
async def claim(*, lease: Duration | None = None) -> ClaimedJob[V] | None: ...
```

Leases the next due job, or None, without waiting.

### Queue.work

```python
async def work(
    handler: Callable[[Job[V]], Awaitable[None]],
    *,
    workers: int = 1,
    timeout: Duration | None = None,
    until_idle: bool = False,
) -> None: ...
```

Runs the queue's jobs as they come due, workers at once, until the task is cancelled.

The server's own Work loop claims ahead and extends leases, and nothing
polls. A lost connection takes the jobs in hand with it, as a process
that died would, and the loop connects again. Cancelled, the loop gives
the jobs its handlers did not finish back without counting them.

<!-- Generated by task reference from sdk/python/src/tinystore/jobs.py. Edit the doc comments there, not this file. -->
