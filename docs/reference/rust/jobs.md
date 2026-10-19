# Jobs API for Rust

Every public type, trait and function of the Jobs engine in `tinystore::jobs`, generated from its source. The [Jobs guide](../../chat/replies.md) explains how to use them, and the [Bun and Node](../bun/jobs.md), [Python](../python/jobs.md) and [Go](../go/jobs.md) pages list the same API.

## Store

```rust
impl Store
```

### Store.queue

```rust
pub fn queue<V>(&self, name: &str) -> QueueBuilder<V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
```

A queue of jobs whose values are `V`, as JSON, in the store's jobs.db;
the engine opens with the first queue.

### Store.schedule

```rust
pub fn schedule(&self, name: &str) -> ScheduleBuilder
```

A schedule: one repeating job under `name`, the code's own. Each time
the program opens it, the code's repeat replaces the one kept.

## JobCall

```rust
pub struct JobCall<'a, V> { /* private fields */ }
```

A call on one job with what it asks beside its value, run by its last
step: `later.id(&key).at(nine_am).add(&draft)`.

### JobCall.id

```rust
pub fn id(self, id: impl AsRef<str>) -> Self
```

The job's id, 1 to 1024 bytes of text: what finds it again, and what
adds it once however many times its caller asks.

### JobCall.at

```rust
pub fn at(self, time: SystemTime) -> Self
```

Runs the job at `time`; a time in the past runs it now.

### JobCall.delay

```rust
pub fn delay(self, span: Duration) -> Self
```

Runs the job `span` from now.

### JobCall.group

```rust
pub fn group(self, group: impl AsRef<str>) -> Self
```

The group whose running jobs the queue's concurrency bounds.

### JobCall.every

```rust
pub fn every(self, span: Duration) -> Self
```

Repeats the job every `span`, each id at a phase of its own within it,
the same across restarts; its first run within the next interval unless
`at` or `delay` says. A repeat needs an id, which alone stops it.

### JobCall.cron

```rust
pub fn cron(self, expr: &str, zone: &str) -> Self
```

Repeats the job by a five-field cron expression on the wall clock of
`zone`, an IANA name, `"UTC"` among them.

### JobCall.add

```rust
pub fn add(self, value: &V) -> Result<bool>
```

Adds the job unless its id is taken: by a job scheduled, waiting,
running or failed, or done while the queue's dedupe keeps it. Says
whether it added one; a job without an id is always added.

### JobCall.set

```rust
pub fn set(self, value: &V) -> Result<()>
```

Makes the id's job this value at this time, whatever it was: a job
waiting is made new, a failed one starts again, and a running one runs
once more after this run, with this value.

### JobCall.update

```rust
pub fn update(self, value: &V) -> Result<bool>
```

Changes the id's job when it has not started: its value, and its time,
group or repeat when the call names them. Says whether it did: a job
that runs, ran or never was answers `false`.

## Maintenance

```rust
pub struct Maintenance {
    pub failed: usize,
    pub done: usize,
    pub ids: usize,
}
```

What one maintenance pass removed.

## maintain

```rust
pub fn maintain(store: &Store) -> Result<Maintenance>
```

Removes what the queues this process opened keep no longer, 10,000 rows a
transaction. The store runs it every minute; a store opened without
background work leaves it to the caller.

## Concurrency

```rust
pub struct Concurrency { /* private fields */ }
```

How many jobs run at once: in all, across every worker of the store, and
in each group. `Concurrency::total(8).group(2)` bounds both;
`Concurrency::default().group(2)` each group alone.

### Concurrency.total

```rust
pub fn total(jobs: u32) -> Concurrency
```

### Concurrency.group

```rust
pub fn group(self, jobs: u32) -> Concurrency
```

At most `jobs` of each group run at once, a group being what an add
named; one customer's backlog cannot hold back the others.

## All

```rust
pub struct All<V> { /* private fields */ }
```

Every job a filter names, a page at a time.

## Claimed

```rust
pub struct Claimed<V> {
    pub value: V,
}
```

A job a program claimed for a loop of its own: its value, its run, and the
lease it settles.

### Claimed.run

```rust
pub fn run(&self) -> &Run
```

### Claimed.settle

```rust
pub fn settle(self, outcome: impl Into<Outcome>) -> Result<()>
```

Settles the job by `outcome`, as a handler's answer does: `()` for done,
or one of the run's answers. A job whose lease ended and that another
claim took since, or that a cancel took, is a `conflict`.

## Queue

```rust
pub struct Queue<V> { /* private fields */ }
```

Jobs of one type in the order of their time. A handle is cheap to clone
and to keep.

### Queue.add

```rust
pub fn add(&self, value: &V) -> Result<bool>
```

Adds a job that runs as soon as a worker is free, and returns once it
is on disk. Adds from many callers share a commit.

### Queue.set

```rust
pub fn set(&self, id: impl AsRef<str>, value: &V) -> Result<()>
```

Makes the job under `id` this value, due now, whatever it was; see
`JobCall::set`.

### Queue.update

```rust
pub fn update(&self, id: impl AsRef<str>, value: &V) -> Result<bool>
```

Gives the job under `id` this value when it has not started, and says
whether it did; see `JobCall::update`.

### Queue.cancel

```rust
pub fn cancel(&self, id: impl AsRef<str>) -> Result<bool>
```

Takes the job under `id`, whatever its state, and says whether there
was one. A running one's handler is told to stop: `run.stopped()` turns
true, and what it returns settles nothing. A repeating job stops.

### Queue.get

```rust
pub fn get(&self, id: impl AsRef<str>) -> Result<Option<Job<V>>>
```

Where the job under `id` is: scheduled, waiting and how many jobs are
ahead of it, running and its progress, failed and why, or done while the
queue's dedupe keeps its id; none for an id with no job.

### Queue.list

```rust
pub fn list(&self, filter: &Filter) -> Result<Page<V>>
```

A page of the queue's jobs; see `Filter`.

### Queue.all

```rust
pub fn all(&self, filter: Filter) -> All<V>
```

Every job `filter` names, a page at a time, holding nothing between
pages; a job written during the walk may or may not be met.

### Queue.id

```rust
pub fn id(&self, id: impl AsRef<str>) -> JobCall<'_, V>
```

A call on the job with this id; see `JobCall`.

### Queue.at

```rust
pub fn at(&self, time: SystemTime) -> JobCall<'_, V>
```

### Queue.delay

```rust
pub fn delay(&self, span: Duration) -> JobCall<'_, V>
```

### Queue.group

```rust
pub fn group(&self, group: impl AsRef<str>) -> JobCall<'_, V>
```

### Queue.work

```rust
pub fn work<F, R, E>(&self, handler: F) -> Result<Worker>
where
    F: Fn(V, &Run) -> Result<R, E> + Send + Sync + 'static,
    R: Into<Outcome>,
    E: Display,
```

Runs `handler` on each job as it falls due, on threads the store starts
for it, as many at once as the queue's concurrency lets, one when it
sets none. It returns at once; see `Worker`.

The handler gets the job's value and its run. `Ok(())` is done; an
answer of the run's says otherwise; an error retries the job after its
backoff, its text kept as the job's error, and so does a panic.

### Queue.run_due

```rust
pub fn run_due<F, R, E>(&self, handler: F) -> Result<usize>
where
    F: Fn(V, &Run) -> Result<R, E> + Sync,
    R: Into<Outcome>,
    E: Display,
```

Runs what is due when it starts and what falls due meanwhile, then
returns how many jobs ran; it never waits for a later job. Tests,
scripts and commands use it.

### Queue.concurrency

```rust
pub fn concurrency(&self, handlers: u32) -> Workers<'_, V>
```

A worker that runs at most `handlers` at once, under the queue's
concurrency.

### Queue.claim

```rust
pub fn claim(&self) -> Result<Option<Claimed<V>>>
```

Takes the next due job for a loop of the program's own, which settles
it with `Claimed::settle`; none when nothing is due. A job not settled
within the queue's timeout goes back to the queue.

### Queue.watch

```rust
pub fn watch(&self, id: impl AsRef<str>) -> Watch<V>
```

The job under `id` as it is, then again each time its state, place,
attempt, time, progress or error changes; it ends with the job: done,
failed or cancelled, the last it yields. An id with no job yields
nothing.

## QueueBuilder

```rust
pub struct QueueBuilder<V> { /* private fields */ }
```

A queue being opened: its name, its type, and how it treats its jobs.

### QueueBuilder.attempts

```rust
pub fn attempts(self, attempts: u32) -> Self
```

Runs a job gets before it fails for good, the first counted: 10.

### QueueBuilder.backoff

```rust
pub fn backoff(self, initial: Duration, max: Duration) -> Self
```

The wait before a retry: `initial`, doubling each time up to `max`, a
tenth longer or shorter by the job: 1 s to 1 h.

### QueueBuilder.timeout

```rust
pub fn timeout(self, timeout: Duration) -> Self
```

How long one run may take before it is told to stop: a minute.

### QueueBuilder.concurrency

```rust
pub fn concurrency(self, concurrency: impl Into<Concurrency>) -> Self
```

Jobs running at once across every worker of the store, `8`, or in all
and in each group, `Concurrency::total(8).group(2)`. Without it each
worker runs one at a time.

### QueueBuilder.rate

```rust
pub fn rate(self, count: u32, per: Duration) -> Self
```

Jobs started in any span of `per`: 30 a second, as an API's limit asks.
It counts starts, retries among them, and lives in memory.

### QueueBuilder.dedupe

```rust
pub fn dedupe(self, span: Duration) -> Self
```

How long a done job keeps its id taken, so that adding it again adds
nothing: an id runs once.

### QueueBuilder.keep

```rust
pub fn keep(self, span: Duration) -> Self
```

How long a failed job stays, with its error, to be found and started
again: a week.

### QueueBuilder.max_waiting

```rust
pub fn max_waiting(self, jobs: u64) -> Self
```

Jobs that may wait; an add past it is a `limit`: ten million.

### QueueBuilder.open

```rust
pub fn open(self) -> Result<Queue<V>>
```

## Workers

```rust
pub struct Workers<'a, V> { /* private fields */ }
```

A worker being started: how many handlers it runs at once.

### Workers.work

```rust
pub fn work<F, R, E>(self, handler: F) -> Result<Worker>
where
    F: Fn(V, &Run) -> Result<R, E> + Send + Sync + 'static,
    R: Into<Outcome>,
    E: Display,
```

Starts the worker; see `Queue::work`.

### Workers.run_due

```rust
pub fn run_due<F, R, E>(self, handler: F) -> Result<usize>
where
    F: Fn(V, &Run) -> Result<R, E> + Sync,
    R: Into<Outcome>,
    E: Display,
```

Runs what is due; see `Queue::run_due`.

## Filter

```rust
pub struct Filter { /* private fields */ }
```

Which jobs a page lists: those whose ids start with a prefix, in the byte
order of their ids, or with no prefix and `Failed` the failed jobs, the
last failed first. A prefix is text: `chat:4` meets chat 42 too, so end it
with a separator.

### Filter.prefix

```rust
pub fn prefix(prefix: impl Into<String>) -> Filter
```

### Filter.state

```rust
pub fn state(self, state: State) -> Filter
```

The jobs in `state` alone: scheduled, waiting, running or failed.

### Filter.limit

```rust
pub fn limit(self, jobs: usize) -> Filter
```

Jobs a page holds at most: 100 unless said, 1000 at most.

### Filter.after

```rust
pub fn after(self, next: impl Into<String>) -> Filter
```

Where the page before ended: its `next`.

## Job

```rust
pub struct Job<V> {
    pub id: Option<String>,
    pub value: V,
    pub state: State,
    pub at: SystemTime,
    pub attempt: u32,
    pub ahead: u32,
    pub progress: Option<Value>,
    pub error: Option<String>,
    pub group: Option<String>,
    pub repeat: Option<String>,
    pub last_run: Option<LastRun>,
}
```

A job as its queue holds it.

## LastRun

```rust
pub struct LastRun {
    pub started_at: SystemTime,
    pub ended_at: SystemTime,
}
```

When the last run a handler finished began and ended.

## PAGE_JOBS

```rust
pub const PAGE_JOBS: usize
```

The most jobs a page holds.

## Page

```rust
pub struct Page<V> {
    pub jobs: Vec<Job<V>>,
    pub next: Option<String>,
}
```

One page of jobs, from one snapshot.

## State

```rust
pub enum State {
    Scheduled,
    Waiting,
    Running,
    Done,
    Failed,
    Cancelled,
}
```

Where a job is in its life.

## Outcome

```rust
pub struct Outcome(_)
```

What a handler answers about its job. `Ok(())` is done; the run's `retry`,
`snooze` and `fail` make the others.

## Run

```rust
pub struct Run { /* private fields */ }
```

This call on a job: when it was due, which attempt it is, whether it was
told to stop, and the answers a handler returns to say what happens next.

### Run.id

```rust
pub fn id(&self) -> Option<&str>
```

The id the job was added under; none for a job added without one.

### Run.at

```rust
pub fn at(&self) -> SystemTime
```

When the run was due, which is earlier than now when it started late: a
handler that reads "the last day" counts it from here.

### Run.attempt

```rust
pub fn attempt(&self) -> u32
```

This run's number among the job's attempts, the first being 1.

### Run.group

```rust
pub fn group(&self) -> Option<&str>
```

### Run.stopped

```rust
pub fn stopped(&self) -> bool
```

Whether the run was told to stop: a cancel took its job, or it ran past
its queue's timeout. A handler that checks it between pieces of work
stops in time; what it did before stays done.

### Run.set_progress

```rust
pub fn set_progress(&self, progress: &impl Serialize) -> Result<()>
```

Shows how far the run got to `get` and `watch`: any JSON up to 4 KiB, a
fraction or a count as the application likes. It lives in memory, and a
run a restart ends starts again from nothing.

### Run.step

```rust
pub fn step<T, E>(&self, name: &str, work: impl FnOnce() -> Result<T, E>) -> Result<T, E>
where
    T: Serialize + DeserializeOwned,
    E: From<Error>,
```

Runs `work` once for the job under `name` and keeps what it answered:
the attempt after a retry, a lost worker or a restart gets the kept
answer back without running `work` again. A step whose attempt ends
before its answer is kept runs again, so what `work` does outside the
store should bear doing twice.

### Run.done

```rust
pub fn done(&self) -> Outcome
```

The answer that says the job is done, for a handler whose other
branches answer `retry` or `fail`; a handler that answers nothing else
returns `Ok(())` instead.

### Run.retry

```rust
pub fn retry(&self, when: impl Into<When>) -> Outcome
```

Runs the job again at `when`, the run counted as an attempt; past the
queue's attempts it fails for good.

### Run.snooze

```rust
pub fn snooze(&self, when: impl Into<When>) -> Outcome
```

Runs the job again at `when`, the run not counted: a provider that asks
for a minute, a row that says later.

### Run.fail

```rust
pub fn fail(&self, reason: impl Display) -> Outcome
```

Fails the job for good now, `reason` kept as its error.

## When

```rust
pub enum When {
    After(Duration),
    At(SystemTime),
}
```

When a retry or a snooze runs the job again: a span from now, or a time.

## Schedule

```rust
pub struct Schedule { /* private fields */ }
```

A repeat the code owns, and the worker running it when it was opened with
one.

### Schedule.get

```rust
pub fn get(&self) -> Result<Option<Job<()>>>
```

When it runs next and how its last run went: what a page of cron jobs
shows.

### Schedule.run_due

```rust
pub fn run_due<F, R, E>(&self, handler: F) -> Result<usize>
where
    F: Fn(&Run) -> Result<R, E> + Sync,
    R: Into<Outcome>,
    E: Display,
```

Runs what is due, as `Queue::run_due` does.

### Schedule.stop

```rust
pub fn stop(&self)
```

Stops the schedule's worker, as `Worker::stop` does.

### Schedule.cancel

```rust
pub fn cancel(&self) -> Result<bool>
```

Removes the schedule's job, until the program opens it again.

## ScheduleBuilder

```rust
pub struct ScheduleBuilder { /* private fields */ }
```

A schedule being opened: its name and its repeat.

### ScheduleBuilder.every

```rust
pub fn every(self, span: Duration) -> Self
```

Runs every `span`, at the schedule's own phase within it.

### ScheduleBuilder.cron

```rust
pub fn cron(self, expr: &str, zone: &str) -> Self
```

Runs by a five-field cron expression on the wall clock of `zone`, an
IANA name, `"UTC"` among them.

### ScheduleBuilder.attempts

```rust
pub fn attempts(self, attempts: u32) -> Self
```

Runs a run gets before it fails, the first counted: 10. A failed run
retries, never past the next time, and the schedule goes on.

### ScheduleBuilder.backoff

```rust
pub fn backoff(self, initial: Duration, max: Duration) -> Self
```

### ScheduleBuilder.timeout

```rust
pub fn timeout(self, timeout: Duration) -> Self
```

### ScheduleBuilder.work

```rust
pub fn work<F, R, E>(self, handler: F) -> Result<Schedule>
where
    F: Fn(&Run) -> Result<R, E> + Send + Sync + 'static,
    R: Into<Outcome>,
    E: Display,
```

Opens the schedule and runs `handler` at each time on a worker of the
store's, as `Queue::work` does; the handler gets the run alone.

### ScheduleBuilder.open

```rust
pub fn open(self) -> Result<Schedule>
```

Opens the schedule without running it: for a program that reads where
it is, or a test that runs it with `run_due`.

## TxQueue

```rust
pub struct TxQueue<'a, V> { /* private fields */ }
```

A queue's calls inside a transaction, `tx.with(&queue)`: its writes and
its reads of one job. A worker, a watch and a page read outside it.

### TxQueue.add

```rust
pub fn add(&self, value: &V) -> Result<bool>
```

Adds a job, as `Queue::add` does, once the transaction commits.

### TxQueue.set

```rust
pub fn set(&self, id: impl AsRef<str>, value: &V) -> Result<()>
```

### TxQueue.update

```rust
pub fn update(&self, id: impl AsRef<str>, value: &V) -> Result<bool>
```

### TxQueue.cancel

```rust
pub fn cancel(&self, id: impl AsRef<str>) -> Result<bool>
```

Takes the job under `id`, as `Queue::cancel` does; a running job's
handler is told to stop once the transaction commits.

### TxQueue.get

```rust
pub fn get(&self, id: impl AsRef<str>) -> Result<Option<Job<V>>>
```

The job under `id` as the transaction sees it, its own writes among it.

### TxQueue.id

```rust
pub fn id(&self, id: impl AsRef<str>) -> JobCall<'a, V>
```

### TxQueue.at

```rust
pub fn at(&self, time: SystemTime) -> JobCall<'a, V>
```

### TxQueue.delay

```rust
pub fn delay(&self, span: Duration) -> JobCall<'a, V>
```

### TxQueue.group

```rust
pub fn group(&self, group: impl AsRef<str>) -> JobCall<'a, V>
```

## Watch

```rust
pub struct Watch<V> { /* private fields */ }
```

A job watched from Rust: the job as it is, then again each time it
changes, until it ends; each `next` waits on the calling thread. Dropped,
it stops watching.

## Worker

```rust
pub struct Worker { /* private fields */ }
```

A worker running a queue's handlers on threads the store started for it.
Dropped, it keeps running until the store closes; `stop` stops it.

### Worker.stop

```rust
pub fn stop(&self)
```

Takes no job more and waits for the handlers under way, each bounded by
its queue's timeout; the store's close stops every worker so.

<!-- Generated by task reference from crates/tinystore through rustdoc. Edit the doc comments there, not this file. -->
