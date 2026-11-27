# Jobs API for Bun and Node

Every public class, function and type of the Jobs engine in `@tinyshed/tinystore`, generated from its source. The [Jobs guide](../../guides/jobs/README.md) explains how to use them, and the [Python](../python/jobs.md), [Go](../go/jobs.md) and [Rust](../rust/jobs.md) pages list the same API.

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
    /** jobs of each group started in any span, beside rate: '2/s', each account's own limit; it lives in memory */
    groupRate?: Rate | undefined
    /** how long a done job's id stays taken, so that an add of it adds nothing: '10m', or { calendar: 'day', timeZone } */
    dedupe?: Dedupe | undefined
    /** how long a failed job stays, to be found and started again: a week; '0s' not at all */
    keep?: Duration | undefined
    /** jobs that may wait; an add past it is LimitError: ten million */
    maxWaiting?: number | undefined
    /** hours in which the queue's jobs do not run: a time inside them, a retry's or a repeat's too, becomes their end */
    quiet?: Quiet | undefined
}
```

## Quiet

```ts
interface Quiet {
    from: string
    to: string
    timeZone: string
}
```

Hours in which a job does not run, two wall-clock times of a day in a time
zone: `{ from: '23:00', to: '07:00', timeZone: 'Europe/Moscow' }`. A from
after to wraps midnight; the same two, or a zone no one has, is InvalidError.

## Dedupe

```ts
type Dedupe = Duration | {
    calendar: Calendar
    timeZone: string
}
```

How long a done job holds its id: a span from when it was done, '10m', or
until the local day, week or month it was done in ends, in its zone:
`{ calendar: 'day', timeZone: 'Europe/Moscow' }`.

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
    /** how long this job holds its id once done, in the place of its queue's dedupe */
    dedupe?: Dedupe | undefined
    /** how long this job is kept once it failed for good, in the place of its queue's: '0s' not at all */
    keep?: Duration | undefined
    /** it leaves its queue this long after its last write unless written again: an upsert, a set or an update renews it */
    ttl?: Duration | undefined
    /** it leaves its queue at this time; not beside ttl */
    expiresAt?: Time | undefined
    /** this job's own hours in which it does not run, in the place of its queue's; false: no hours hold it */
    quiet?: Quiet | false | undefined
}
```

A job's time, group, repeat and options of its own, as an add, a set and an update take them.

## AddOptions

```ts
interface AddOptions extends JobOptions {
    /** the job's id, 1 to 1024 bytes: what finds it again, and adds it once */
    id?: string | undefined
}
```

## GatherOptions

```ts
interface GatherOptions {
    /** the job runs this long after the last gather under its id */
    settle: Duration
    /** and never later than this long after the gather that started its list */
    max?: Duration | undefined
    /** the group whose running jobs the queue's concurrency.group bounds */
    group?: string | undefined
    /** how long this job holds its id once done, in the place of its queue's dedupe */
    dedupe?: Dedupe | undefined
    /** how long this job is kept once it failed for good, in the place of its queue's: '0s' not at all */
    keep?: Duration | undefined
    /** it leaves its queue this long after its last write unless written again */
    ttl?: Duration | undefined
    /** it leaves its queue at this time; not beside ttl */
    expiresAt?: Time | undefined
    /** this job's own hours in which it does not run, in the place of its queue's; false: no hours hold it */
    quiet?: Quiet | false | undefined
}
```

How a gather waits, and the options of its own a job it adds or changes takes.

## Gathered

```ts
interface Gathered {
    added: boolean
    /** 0 when a done job its dedupe holds keeps the id, and nothing was gathered */
    count: number
    at: Date
}
```

What a gather did: whether it started the job, the items it holds now, and when it is due.

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
    /** when it leaves its queue unless written again: its ttl's end, or its expiresAt */
    expiresAt: Date | undefined
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

## JobCounts

```ts
interface JobCounts {
    /** due later */
    scheduled: number
    /** due, and no handler runs it yet */
    waiting: number
    running: number
    /** kept for the queue's keep */
    failed: number
}
```

How many jobs a queue holds in each state, from one snapshot; a done job has left it.

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

Adds a job unless its id is taken: by a job scheduled, waiting or
running, or done while the queue's dedupe keeps it. A job failed for
good holds no id: the new job takes its place. It returns once the job
is on disk, and says whether it added one; a job without an id is
always added.

    await reminders.add({ userId: 42, text: 'Call mom' }, { delay: '1h' })

### Queue.adding

```ts
adding(value: T, options?: AddOptions): Adding
```

A job's add for a batch of the database the queue was opened from:
`db.batch` writes it with the batch's statements, so the job commits
with the rows or not at all, and shares the commit with every other
write of the file, where `db.tx` holds the writer and pays a sync of
its own. It adds as `add` does, unless its id is taken: its `Done` in
the batch's answer changed 1 job, or 0.

    await db.batch([sql`update notes set body = ${body} where id = ${id}`, indexing.adding({ noteId: id })])

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

Changes the id's job when it is not running: its value, the steps kept
for the old one dropped with it, and its time, group or repeat when the
options give them. It says whether it did: a job that runs, is done or
failed, or never was answers false.

### Queue.upsert

```ts
upsert(id: string, value: T, options?: JobOptions): Promise<boolean>
```

Adds the job under `id` when it is free, as `add` does, or brings the
one there to this call: its value, group, repeat and options of its own
where they differ, its time, attempts and steps as they were. A changed
value drops the steps kept for the old one; a changed repeat moves a job
not yet due to the repeat's next time. It says whether it added or
changed anything: a job that already was so is not written, so it is
cheap to call on every request. `at` and `delay` count only when it adds.

    const changed = await polls.upsert(`group:${g}`, { group: g }, { every: '3m' })

### Queue.gather

```ts
gather<I>(this: Queue<I[]>, id: string, items: I[], options: GatherOptions): Promise<Gathered>
```

Gathers items into the job under id, whose value is a list: a job
scheduled or waiting takes them after its own, in the order they came,
and is due settle after this gather, never later than max after the
gather that started it. With no job there it adds one of these items;
a running one runs on with what it read, and its next run takes only
what is gathered meanwhile, at its settle time, once it ends well; a
retry takes both, and a failure for good leaves the gathered ones a job
of their own. A done job its dedupe holds keeps the id, count 0; a
failed one gives it up, as to add. A
value that is no list, there or here, or a job that repeats, is
InvalidError. It compiles only for a queue whose value is a list.

    const batch = store.queue<Notice[]>('notify-batch')
    const { added, count, at } = await batch.gather('grades:7', [{ course: 1 }], { settle: '5m', max: '1h' })

### Queue.cancel

```ts
cancel(job: Job<T> | string): Promise<boolean>
```

Takes a job, whatever its state, and says whether it was there: the job
under an id, or a job a list gave, by its id or, for one without, by the
row it was listed from. A job retried, run again for its repeat or
cancelled since is in no row, and its cancel takes nothing, never another
job. A running one's handler sees its signal abort, and what it returns
then settles nothing; a repeating one stops.

    for await (const job of emails.all({ state: 'scheduled' })) if (job.value.to === bounced) await emails.cancel(job)

### Queue.counts

```ts
counts(): Promise<JobCounts>
```

How many jobs the queue holds in each state, from one snapshot; it reads no value.

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

A page of the queue's jobs: with no prefix every job, those with ids by id,
then those without as they run, then those without that failed.

### Queue.all

```ts
all(options?: Omit<ListOptions, 'after'>): AsyncGenerator<Job<T>>
```

Every job the options name, a page at a time, holding nothing between
pages. A job whose value no longer reads is a CorruptError in its place,
naming it, and iterating the same walk again goes on past it.

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

### TxQueue.upsert

```ts
upsert(id: string, value: T, options?: JobOptions): Promise<boolean>
```

### TxQueue.gather

```ts
gather<I>(this: TxQueue<I[]>, id: string, items: I[], options: GatherOptions): Promise<Gathered>
```

Gathers items into the job under id, as `queue.gather` does, with the transaction.

### TxQueue.cancel

```ts
cancel(job: Job<T> | string): Promise<boolean>
```

### TxQueue.get

```ts
get(id: string): Promise<Job<T> | undefined>
```

The job under id as the transaction sees it, its own writes among it.

## Adding

```ts
class Adding {}
```

A job's add that a database's batch carries, from `queue.adding`: it commits with the batch's statements, or not at all.

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

Changes the id's job when it is not running, its old value's steps dropped, with the commit.

### StoreTxQueue.upsert

```ts
upsert(id: string, value: T, options?: JobOptions): Promise<void>
```

Adds the job under id, or brings the one there to this call, its time kept, with the commit.

### StoreTxQueue.cancel

```ts
cancel(job: Job<T> | string): Promise<void>
```

Takes the job under id, or a job a list gave, whatever its state, with the commit.

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

### Schedule.runNow

```ts
runNow(): Promise<void>
```

Runs the schedule once as soon as a worker is free, under its own
rules, and leaves its later times as they were: run at 03:29, a
schedule of 03:30 runs again at 03:30. A run under way is followed by
one more right after it, never beside it. A schedule whose job a cancel
took is `NotFoundError` until the program opens it again.

    if (!(await night.get())?.lastRun) await night.runNow() // the first deploy: run once if it never ran

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
