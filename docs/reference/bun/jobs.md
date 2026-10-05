# Jobs API for Bun and Node

Every public class, function and type of the Jobs engine in `@tinyshed/tinystore`, generated from its source. The [Jobs guide](../../jobs/README.md) explains how to use them, and the [Python](../python/jobs.md) and [Go](../go/jobs.md) pages list the same API.

## QueueOptions

```ts
interface QueueOptions {
    /** how long a claim holds a job before it is given back: 30 s */
    lease?: Duration
    /** the attempts after which a job fails for good: 10 */
    maxAttempts?: number
    /** a retry's wait, doubling from first to most: a second to an hour */
    backoff?: {
        first?: Duration
        most?: Duration
    }
    /** the jobs a queue holds before it refuses the next with LimitError: ten million */
    maxWaiting?: number
    /** how long a failed job is kept for Get and Scan: seven days */
    keepFailed?: Duration
    /** remembers a done job's key this long, so that enqueuing it again adds nothing */
    keepDone?: Duration
    /** the jobs that may run at once, across every worker of the store: no bound */
    maxRunning?: number
    /** the jobs of one group, its enqueue's `group`, that may run at once: no bound */
    maxRunningInGroup?: number
    /** how many jobs may start in any span, across every worker of the store: `'30/s'` */
    rate?: Rate
    /**
     * keeps the queue in this database's file instead of jobs.db, so that a
     * batch of the database commits a job with its rows: `queue.withTx(tx)`
     */
    in?: Database
}
```

## Repeat

```ts
type Repeat = {
    cron: string
    zone: string
} | {
    daily: string
    zone: string
} | {
    every: Duration
    spread?: boolean
}
```

When a job runs again: cron text in a zone by its name, a daily time, or
every so often, which `spread` runs at a phase of each key's own.

## EnqueueOptions

```ts
interface EnqueueOptions {
    /** the job's time; one past runs now */
    at?: Time
    after?: Duration
    /** names one job: enqueuing it again adds nothing and can bring it forward, never back */
    key?: string
    /** needs a key, since only a key stops it */
    repeat?: Repeat
    /** sets the time of the key's job either way, later too: a running one runs again then; needs a key */
    move?: boolean
    /** the group whose running jobs the queue's `maxRunningInGroup` bounds */
    group?: string
}
```

## JobState

```ts
type JobState = 'waiting' | 'running' | 'failed' | 'done' | 'cancelled'
```

done while keepDone keeps its key; cancelled only a watcher sees

## JobEntry

```ts
interface JobEntry<T> {
    key: string
    value: T
    /** the time it runs for */
    at: Date
    /** its attempts, a running one included */
    attempt: number
    state: JobState
    /** the jobs that run before a waiting one, up to 10,000; get and watch count it, scan leaves it 0 */
    ahead: number
    /** what a running job's handler last reported with job.progress */
    progress: unknown
    /** when the last run a handler finished began: acknowledged, retried, failed or snoozed */
    ran: Date | undefined
    /** how many milliseconds that run took */
    took: number | undefined
    /** its last failure */
    error: string | undefined
    /** a repeating job's cron text and zone */
    repeat: string | undefined
}
```

## WorkOptions

```ts
interface WorkOptions {
    /** the jobs in this process's hands at once: 1, the one order a queue promises */
    workers?: number
    /** how long a job may be in the handler's hands, after which its attempt fails: a minute */
    timeout?: Duration
    /** stops taking jobs: the ones in hand finish first, and work returns */
    signal?: AbortSignal
    /** returns once no job is due and none runs, as a test wants it */
    untilIdle?: boolean
}
```

## Job

```ts
class Job<T> {
    readonly key: string
    readonly value: T
    readonly at: Date
    readonly attempt: number
    readonly signal: AbortSignal
    settlement: Settlement | undefined
}
```

A job in a work loop's handler. Returning acknowledges it and throwing
retries it, unless the handler said otherwise with retry, fail or snooze;
the last of those it calls is how the job settles.

### Job.step

```ts
step<R>(name: string, fn: () => R | Promise<R>): Promise<R>
```

Runs fn once in the job's run and keeps what it answers, as JSON: an
attempt after a retry, a lost lease or a restart gets the kept answer
back without running fn again. A step whose attempt ends before its
answer is kept runs again, so what fn does outside the store should bear
doing twice. A name is the step's within the run, which a loop numbers:
'model:1', 'tool:1', 'model:2'. An answer is at most 1 MiB of JSON.

    const hits = await job.step('search', () => search(job.value.query))

### Job.progress

```ts
progress(progress: unknown): void
```

Reports how far the job got, any JSON within 4 KiB: get and watch show
the latest until the job is settled. It does not wait for the server.

    await transcode({ signal: job.signal, onProgress: p => job.progress(p) })

### Job.retry

```ts
retry(error?: unknown, when?: {
        at?: Time
        after?: Duration
    }): void
```

Fails this attempt: the job runs again after its backoff, or at the time given.

### Job.fail

```ts
fail(error?: unknown): void
```

Fails the job for good, keeping it for keepFailed with its reason.

### Job.snooze

```ts
snooze(when: {
        at?: Time
        after?: Duration
    }): void
```

Moves the job to another time without counting the attempt.

## ClaimedJob

```ts
class ClaimedJob<T> {
    readonly key: string
    readonly value: T
    readonly at: Date
    readonly attempt: number
}
```

A job a claim leased, which its caller settles itself before the lease ends.

### ClaimedJob.step

```ts
step<R>(name: string, fn: () => R | Promise<R>): Promise<R>
```

Runs fn once in the job's run and keeps its answer, as a work loop's job.step does.

### ClaimedJob.ack

```ts
ack(): Promise<void>
```

The job is done.

### ClaimedJob.retry

```ts
retry(error?: unknown, when?: {
        at?: Time
        after?: Duration
    }): Promise<void>
```

### ClaimedJob.fail

```ts
fail(error?: unknown): Promise<void>
```

### ClaimedJob.snooze

```ts
snooze(when: {
        at?: Time
        after?: Duration
    }): Promise<void>
```

### ClaimedJob.extend

```ts
extend(d: Duration): Promise<void>
```

Holds the job this much longer from now.

### ClaimedJob.progress

```ts
progress(progress: unknown): Promise<void>
```

Reports how far the job got, any JSON within 4 KiB, which get and watch show until it is settled.

## Jobs

```ts
class Jobs {}
```

### Jobs.queue

```ts
queue<T = unknown>(name: string, options?: QueueOptions): Queue<T>
queue<S extends StandardSchemaV1>(name: string, schema: S, options?: QueueOptions): Queue<StandardSchemaV1.InferOutput<S>>
```

A queue of JSON values of type T.

### Jobs.schedule

```ts
schedule(name: string, repeat: Repeat, options?: QueueOptions): Queue<null>
```

A schedule: a queue of one job under the schedule's name, which repeats
as it says. work runs it; cancel stops it.

## QueueTx

```ts
class QueueTx<T> {}
```

A queue's enqueue inside a batch of the database it lives in.

### QueueTx.enqueue

```ts
enqueue(value: T, options?: EnqueueOptions): Promise<void>
```

Adds a job to the batch; it settles once the batch has committed.

## Queue

```ts
class Queue<T> {
    readonly name: string
}
```

### Queue.withTx

```ts
withTx(tx: SqlBatch): QueueTx<T>
```

The queue's enqueue inside a batch of the database it lives in, which
it was opened `in`: the job commits with the batch's rows or not at all.

    await db.batch(tx => {
      tx.exec`update notes set body = ${body} where id = ${id}`
      index.withTx(tx).enqueue({ id })
    })

### Queue.enqueue

```ts
enqueue(value: T, options?: EnqueueOptions): Promise<void>
```

Adds a job; it returns once the job is in the file.

### Queue.enqueueAll

```ts
enqueueAll(jobs: readonly ({
        value: T
    } & EnqueueOptions)[]): Promise<void>
```

Adds jobs in one transaction, all or none: a refused one names itself as `call`.

### Queue.update

```ts
update(key: string, value: T, options?: Omit<EnqueueOptions, 'key' | 'move'>): Promise<void>
```

Changes a job that waits or failed: its value and, when the options
say, its time, repeat or group. A job a worker holds, done or absent is
ConflictError.

### Queue.cancel

```ts
cancel(key: string): Promise<boolean>
```

Removes the job under key, whether it waits, runs or failed, and says
whether there was one. A running job's handler sees job.signal abort,
and what it returns settles nothing.

### Queue.get

```ts
get(key: string): Promise<JobEntry<T> | undefined>
```

The job a key names, waiting, running or failed, or done while keepDone
keeps its key; undefined when none is.

### Queue.watch

```ts
watch(key: string): AsyncGenerator<JobEntry<T>>
```

Yields the job under key as it is, then again each time its state,
place, attempt, time, progress or error changes, until it ends: done,
failed or cancelled, the last entry it yields. A key that names no job
yields nothing. Leaving the loop ends the watch; a connection lost is
connected again, and the watch goes on from the job as it is then.

    for await (const s of videos.watch(id)) send(s.state, s.ahead, s.progress)

### Queue.scan

```ts
scan(query?: {
        prefix?: string
        state?: 'failed'
        after?: string
        limit?: number
    }): Promise<{
        items: JobEntry<T>[]
        next: string | undefined
    }>
```

A page of the jobs under a prefix, in the byte order of their keys; with
state 'failed' and no prefix, the failed jobs, the last failed first.

### Queue.all

```ts
all(query?: {
        prefix?: string
        state?: 'failed'
        limit?: number
    }): AsyncGenerator<JobEntry<T>>
```

Walks scan's pages, holding no snapshot between them.

### Queue.claim

```ts
claim(options?: {
        lease?: Duration
    }): Promise<ClaimedJob<T> | undefined>
```

Leases the next due job, or says there is none, without waiting. The
claim lives on this connection: settle it before its lease ends.

### Queue.work

```ts
work(handler: (job: Job<T>) => void | Promise<void>, options?: WorkOptions): Promise<void>
```

Runs the queue's jobs as they come due, workers at once, until the
signal aborts: the server's own Work loop claims ahead and extends
leases, and nothing polls. A handler's return acknowledges its job and a
throw retries it. A connection lost takes the jobs in hand with it, as a
process that died would, and the loop connects again.

<!-- Generated by task reference from sdk/js/src/jobs.ts. Edit the doc comments there, not this file. -->
