# Jobs API for Bun and Node

Every public class, function and type of the Jobs engine in `@tinyshed/tinystore`, generated from its source. The [Jobs guide](../../jobs/README.md) explains how to use them, and the [Python](../python/jobs.md) and [Go](../go/jobs.md) pages list the same API.

## Concurrency

```ts
interface Concurrency {
    total?: number | undefined
    /** at most this many jobs of one group at once: one customer's backlog holds back no other */
    group?: number | undefined
}
```

How many jobs of a queue run at once: in all, across every worker of the store, and in each group.

## Backoff

```ts
interface Backoff {
    initial: Duration
    max: Duration
}
```

The wait before a retry: initial, doubling each time up to max, a tenth longer or shorter at random.

## QueueOptions

```ts
interface QueueOptions {
    /** runs a job gets before it fails for good, the first counted: 10 */
    attempts?: number | undefined
    /** 1s doubling up to 1h */
    backoff?: Backoff | undefined
    /** how long one run may take before its signal aborts: a minute */
    timeout?: Duration | undefined
    /** jobs running at once across every worker of the store: 8, or { total: 8, group: 2 }; one a worker unless given */
    concurrency?: number | Concurrency | undefined
    /** jobs started in any span: '30/s' */
    rate?: Rate | undefined
    /** how long a done job's id stays taken, so that an add of it adds nothing */
    dedupe?: Duration | undefined
    /** how long a failed job stays, to be found and started again: a week */
    keep?: Duration | undefined
    /** jobs that may wait; an add past it is LimitError: ten million */
    maxWaiting?: number | undefined
}
```

## JobOptions

```ts
interface JobOptions {
    /** runs at this time, one past now; not beside delay */
    at?: Time | undefined
    /** runs this long from now */
    delay?: Duration | undefined
    /** the group whose running jobs the queue's concurrency.group bounds */
    group?: string | undefined
    /** repeats every span, each id at a phase of its own within it; needs an id */
    every?: Duration | undefined
    /** repeats by five cron fields, or @daily and its kind, on timeZone's wall clock; needs an id */
    cron?: string | undefined
    /** an IANA name, 'UTC' among them, which a cron needs: one given as undefined is InvalidError, never UTC */
    timeZone?: string | undefined
}
```

A job's time, group and repeat, as an add, a set and an update take them.

## AddOptions

```ts
interface AddOptions extends JobOptions {
    /** the job's id, 1 to 1024 bytes: what finds it again, and adds it once */
    id?: string | undefined
}
```

## JobState

```ts
type JobState = 'scheduled' | 'waiting' | 'running' | 'done' | 'failed' | 'cancelled'
```

done while dedupe keeps its id; cancelled only a watcher sees

## Job

```ts
interface Job<T> {
    id: string | undefined
    value: T
    state: JobState
    /** when it runs next; for one done or failed, when its last run was for */
    at: Date
    /** its attempts, a running one included */
    attempt: number
    /** the jobs that run before a waiting one, up to 10,000 */
    ahead: number
    /** what its handler last reported of a running job */
    progress: unknown
    /** why its last run failed */
    error: string | undefined
    group: string | undefined
    /** a repeating job's repeat as it keeps it: '@every 30s +6178ms', '10 3 * * * Europe/Berlin' */
    repeat: string | undefined
    /** when the last run a handler finished started and ended */
    lastRun: {
        startedAt: Date
        endedAt: Date
    } | undefined
}
```

A job as its queue holds it.

## ListOptions

```ts
interface ListOptions {
    /** ids that start with it, in their byte order: end it with a separator, 'chat:4:' */
    prefix?: string | undefined
    /** the jobs in this state alone; 'failed' with no prefix lists the last failed first */
    state?: 'scheduled' | 'waiting' | 'running' | 'failed' | undefined
    /** where the page before ended: its next */
    after?: string | undefined
    /** jobs a page holds at most: 100, 1000 at most */
    limit?: number | undefined
}
```

## JobPage

```ts
interface JobPage<T> {
    jobs: Job<T>[]
    next: string | undefined
}
```

A page of a queue's jobs, and where the next starts when there is one.

## WorkOptions

```ts
interface WorkOptions {
    /** the handlers this worker runs at once, under the queue's bound: its total, or one */
    concurrency?: number | undefined
}
```

## Handler

```ts
type Handler<T> = (value: T, run: Run) => unknown
```

What runs a job: it gets the job's value and its run. Returning ends the
job done, throwing runs it again after its backoff, and returning what
run.retry, run.snooze or run.fail made ends it so.

## ScheduleWhen

```ts
type ScheduleWhen = {
    every: Duration
} | {
    cron: string
    timeZone: string
}
```

When a schedule runs: every span, or by a cron on a time zone's wall clock.

## ScheduleOptions

```ts
interface ScheduleOptions {
    /** runs a time gets before it fails, the first counted: 10; the schedule goes on */
    attempts?: number | undefined
    backoff?: Backoff | undefined
    /** how long one run may take before its signal aborts: a minute */
    timeout?: Duration | undefined
}
```

## Answer

```ts
class Answer {}
```

How a run ends otherwise than done, made by run.retry, run.snooze or run.fail: its handler returns it.

### Answer.toString

```ts
toString(): string
```

## Run

```ts
class Run {
    readonly id: string | undefined
    readonly at: Date
    readonly attempt: number
    readonly group: string | undefined
    readonly signal: AbortSignal
}
```

This call on a job: what it is, its signal, its steps and progress, and the answers its handler returns.

### Run.step

```ts
step<R>(name: string, fn: () => R | Promise<R>): Promise<R>
```

Runs fn once in the job's run and keeps its answer, as JSON, across the
job's attempts: the attempt after a retry, a lost worker or a restart
gets the answer back without running fn again. A step whose attempt
ends before its answer is kept runs again, so what fn does outside the
store should bear doing twice. A name is the step's within its job,
numbered in a loop: 'model:1', 'tool:1'.

    const found = await run.step('search', () => search(question.text))

### Run.setProgress

```ts
setProgress(progress: unknown): void
```

Shows how far the job got, any JSON within 4 KiB, to get: the latest
goes, ten a second at most, and nothing waits for the server.

### Run.retry

```ts
retry(when?: Duration | Date): Answer
```

Runs the job again after its backoff, or at the time given, the run counted as an attempt: return it.

### Run.snooze

```ts
snooze(when: Duration | Date): Answer
```

Runs the job again at the time given, the run not counted: return it.

### Run.fail

```ts
fail(reason: string | Error): Answer
```

Fails the job for good now, its reason kept as its error: return it.

## Queue

```ts
class Queue<T> {
    readonly name: string
}
```

A queue of jobs of one type, run in the order of their time: open it with `store.queue(name)`.

### Queue.add

```ts
add(value: T, options?: AddOptions): Promise<boolean>
```

Adds a job unless its id is taken: by a job scheduled, waiting, running
or failed, or done while the queue's dedupe keeps it. It returns once
the job is on disk, and says whether it added one; a job without an id
is always added.

    await reminders.add({ userId: 42, text: 'Call mom' }, { delay: '1h' })

### Queue.set

```ts
set(id: string, value: T, options?: JobOptions): Promise<void>
```

Makes the id's job this value at this time, whatever it was: one waiting
is made new, a failed one starts again, and a running one runs once more
after this run, with this value.

    await checks.set(`check:${check.id}`, check, { delay: '25h' })

### Queue.update

```ts
update(id: string, value: T, options?: JobOptions): Promise<boolean>
```

Changes the id's job when it has not started: its value, and its time,
group or repeat when the options give them. It says whether it did: a
job that runs, ran or never was answers false.

### Queue.cancel

```ts
cancel(id: string): Promise<boolean>
```

Takes the job under id, whatever its state, and says whether there was
one. A running one's handler sees its signal abort, and what it returns
then settles nothing; a repeating one stops.

### Queue.get

```ts
get(id: string): Promise<Job<T> | undefined>
```

Where the job under id is: scheduled, waiting and how many jobs are
ahead of it, running and its progress, failed and why, or done while
the queue's dedupe keeps its id; undefined for an id with no job.

### Queue.watch

```ts
watch(id: string): AsyncGenerator<Job<T>>
```

The job under id as it is, then again each time its state, place,
attempt, time, progress or error changes; it ends with the job: done,
failed, or cancelled, a state only a watcher sees. An id with no job
yields nothing. A watch whose connection is lost watches again on the
next; a job that ended meanwhile ends it without its last state.

    for await (const job of videos.watch(id)) send(job.state, job.ahead, job.progress)

### Queue.list

```ts
list(options?: ListOptions): Promise<JobPage<T>>
```

A page of the queue's jobs whose ids start with a prefix, or of its failed ones.

### Queue.all

```ts
all(options?: Omit<ListOptions, 'after'>): AsyncGenerator<Job<T>>
```

Every job the options name, a page at a time, holding nothing between pages.

### Queue.work

```ts
work(handler: Handler<T>, options?: WorkOptions): Worker
```

Runs handler on each job as it falls due, as many at once as the
queue's concurrency lets, one when it sets none, and returns the running
worker at once: stop it at shutdown, as store.close() does. A worker
whose connection is lost reaches the store again, the jobs it held
running again as a dead worker's would.

    const worker = reminders.work(async ({ userId, text }) => push(userId, text))

### Queue.runDue

```ts
runDue(handler: Handler<T>, options?: WorkOptions): Promise<number>
```

Runs what is due when it starts and what falls due meanwhile, then says
how many jobs ran; it never waits for a later job. Tests, scripts and
commands use it.

## TxQueue

```ts
class TxQueue<T> {
    readonly name: string
}
```

A queue's calls inside a database's transaction, from `tx.with(queue)`: a
job it adds is there only if the transaction commits, and the queue's
workers find it once it has. A worker, a watch and a page work outside.

### TxQueue.add

```ts
add(value: T, options?: AddOptions): Promise<boolean>
```

Adds a job unless its id is taken, as `queue.add` does, with the transaction.

### TxQueue.set

```ts
set(id: string, value: T, options?: JobOptions): Promise<void>
```

### TxQueue.update

```ts
update(id: string, value: T, options?: JobOptions): Promise<boolean>
```

### TxQueue.cancel

```ts
cancel(id: string): Promise<boolean>
```

### TxQueue.get

```ts
get(id: string): Promise<Job<T> | undefined>
```

The job under id as the transaction sees it, its own writes among it.

## StoreTxQueue

```ts
class StoreTxQueue<T> {
    readonly name: string
}
```

A queue's writes inside the store's transaction, from `tx.with(queue)` in
`store.tx`: each is kept for the commit, which makes them all or none, so
what one did is known once the transaction has committed.

    await store.tx(async tx => {
      for (const member of members) await tx.with(pushes).add({ member, message }, { id: `${message}:${member}` })
    })

### StoreTxQueue.add

```ts
add(value: T, options?: AddOptions): Promise<void>
```

Adds a job unless its id is taken, with the commit.

### StoreTxQueue.set

```ts
set(id: string, value: T, options?: JobOptions): Promise<void>
```

Makes the id's job this value at this time, whatever it was, with the commit.

### StoreTxQueue.update

```ts
update(id: string, value: T, options?: JobOptions): Promise<void>
```

Changes the id's job when it has not started, with the commit.

### StoreTxQueue.cancel

```ts
cancel(id: string): Promise<void>
```

Takes the job under id, whatever its state, with the commit.

## Schedule

```ts
class Schedule {
    readonly name: string
}
```

A repeat the code owns: one repeating job under its name, and the worker that runs it.

### Schedule.get

```ts
get(): Promise<Job<null> | undefined>
```

When it runs next and how its last run went: what a page of cron jobs shows.

### Schedule.runDue

```ts
runDue(): Promise<number>
```

Runs the schedule's job when it is due, as a queue's runDue does.

### Schedule.stop

```ts
stop(): Promise<void>
```

Stops the schedule's worker, as a worker's stop does.

### Schedule.cancel

```ts
cancel(): Promise<boolean>
```

Removes the schedule's job, until the program opens it again.

## Worker

```ts
class Worker implements AsyncDisposable {}
```

A worker running a queue's handlers here while the server's loop claims their jobs.

### Worker.stop

```ts
stop(): Promise<void>
```

Takes no job more and waits for the handlers under way, each bounded by
its timeout: the server gives back the jobs it held for them, and ends
the worker once their answers are written.

### Worker.[Symbol.asyncDispose]

```ts
[Symbol.asyncDispose](): Promise<void>
```

<!-- Generated by task reference from sdk/js/src/jobs.ts. Edit the doc comments there, not this file. -->
