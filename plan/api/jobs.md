# jobs: the API book

Draft, 9 October 2026, after its first newcomer check and blind comparison
(below). Work an application must do later, or now but outside the request
that asked for it: a reminder at six, an email in the background, a push to
every member of a group, an account deleted thirty days after its owner
asked, a cleanup every night. This book is the API before the code;
[dx.md](../dx.md) has the rules it follows, and [kv.md](kv.md) the words both
books share. TypeScript comes first; Python, Go and Rust follow where they
spell something differently. Nothing of it is built in Rust yet.

## What a newcomer learns

| Concept                        | In one line                                                                   |
|--------------------------------|-------------------------------------------------------------------------------|
| queue                          | jobs of one type, run in the order of their time                              |
| job and its `id`               | a value to work on; an id names it, so that adding it twice adds it once      |
| `delay`, `at`, `every`, `cron` | when it runs: after a span, at a time, every interval, on the wall clock      |
| `work`                         | the handler: it returns, the job is done; it throws, the job runs again later |
| `attempts`                     | how many runs a job gets before it fails for good, the first among them       |
| `concurrency` and `rate`       | how many run at once, in all and in each group; how many start a second       |
| `dedupe` and `keep`            | how long a done id blocks adding it again; how long a failed job is kept      |

A job runs at least once: a process that dies after the work and before the
job was marked done runs it again. A handler keys its effect by what the job
names, `insert … on conflict do nothing`, so that twice is harmless.

## Open a queue

```ts
const reminders = store.queue<Reminder>('reminders')
const emails = store.queue<Email>('emails', { attempts: 20, backoff: { initial: '5s', max: '30m' } })
const refreshes = store.queue<Refresh>('refreshes', { concurrency: { total: 8, perGroup: 2 } })
const telegram = store.queue<Message>('telegram', { rate: '30/s' })
const pushes = store.queue<Push>('pushes', { dedupe: '1h' })
```

```python
reminders = store.queue("reminders", Reminder)
emails = store.queue("emails", Email, attempts=20, backoff=Backoff(initial="5s", max="30m"))
refreshes = store.queue("refreshes", Refresh, concurrency=Concurrency(total=8, per_group=2))
```

```go
reminders, err := jobs.Queue[Reminder](store, "reminders")
emails, err := jobs.Queue[Email](store, "emails", jobs.Attempts(20), jobs.Backoff{Initial: 5 * time.Second, Max: 30 * time.Minute})
refreshes, err := jobs.Queue[Refresh](store, "refreshes", jobs.Concurrency{Total: 8, PerGroup: 2})
```

```rust
let reminders = store.queue::<Reminder>("reminders").open()?;
let emails = store.queue::<Email>("emails").attempts(20).backoff(Duration::from_secs(5), Duration::from_mins(30)).open()?;
let refreshes = store.queue::<Refresh>("refreshes").concurrency(8).per_group(2).open()?;
let telegram = store.queue::<Message>("telegram").rate(30, Duration::from_secs(1)).open()?;
```

| Option        | Default                        | What it does                                                                                            |
|---------------|--------------------------------|---------------------------------------------------------------------------------------------------------|
| `attempts`    | 10                             | runs a job gets before it fails for good, the first run counted                                         |
| `backoff`     | `{ initial: '1s', max: '1h' }` | the wait before a retry: `initial`, doubling each time up to `max`, a tenth longer or shorter at random |
| `timeout`     | `'1m'`                         | how long one run may take before it is told to stop and fails                                           |
| `concurrency` | none                           | jobs running at once across every worker of the store: `8`, or `{ total: 8, perGroup: 2 }`              |
| `rate`        | none                           | jobs started in a span: `'30/s'`                                                                        |
| `dedupe`      | none                           | how long a done job's id makes `add` add nothing                                                        |
| `keep`        | `'7d'`                         | how long a failed job stays, with its error, to be found and started again                              |
| `maxWaiting`  | 10,000,000                     | jobs that may wait; an `add` past it is `limit`                                                         |

A value is JSON, so a worker in another language reads it. A queue opened
from an SQL database, `db.queue('indexing')`, keeps its jobs in that
database's file, so that a job commits with the rows it is about.

## Add a job

```ts
await reminders.add({ userId: 42, text: 'Call mom' }, { at: evening })
await reminders.add({ userId: 42, text: 'Drink water' }, { delay: '1h' })
const added = await later.add(draft, { id: `chat:${chatId}:${draft.id}`, at: nineAm })   // false: it was there
await refreshes.add({ dataset: 3 }, { id: 'refresh:3', group: 'db:42' })
```

```rust
reminders.add(&Reminder { user_id: 42, text: "Call mom".into() })?;          // now
reminders.at(evening).add(&reminder)?;
reminders.delay(Duration::from_hours(1)).add(&reminder)?;
let added: bool = later.id(&format!("chat:{chat_id}:{}", draft.id)).at(nine_am).add(&draft)?;
refreshes.id("refresh:3").group("db:42").add(&Refresh { dataset: 3 })?;
```

- `add` returns once the job is on disk, and says whether it added one. A job
  without a time runs as soon as a worker is free; a time in the past runs now.
  Jobs run in the order of their time, equal times in the order they were
  added. `at` and `delay` together are `invalid`.
- An `id` names one job, 1 to 1024 bytes of text. `add` of an id whose job is
  there, scheduled, waiting or failed, changes nothing and returns `false`: a
  user who taps "send" twice adds one job. To change it, `set` or `update` it.
  `add` of an id whose job runs now adds one run more after it, with the new
  value, since the run under way may have read what changed.
- With `dedupe: '1h'` an id stays an hour after its job is done, and `add`
  of it changes nothing: an id runs once.
- In Rust, a job's options are a chain that ends in its verb, as kv's `key()`
  is: `queue.id(…)`, `.at(…)` or `.delay(…)`, `.group(…)`, `.every(…)` or
  `.cron(…)`, then `.add(&value)`.

## Set, update and cancel

```ts
await checks.set(`check:${check.id}`, check, { delay: '25h' })   // the alarm moves on at each ping
await later.update(id, edited, { at: tenAm })   // ConflictError: it is running, or it is done
const cancelled = await later.cancel(id)        // false: it ran already
```

```rust
checks.id(&format!("check:{}", check.id)).delay(Duration::from_hours(25)).set(&check)?;
later.id(&id).at(ten_am).update(&edited)?;
let cancelled: bool = later.id(&id).cancel()?;
```

- `set` makes the id's job this value at this time, whatever it was: it adds
  the job when there is none, replaces a scheduled, waiting or failed one, and
  arms again an id `dedupe` keeps. A job running now runs once more after it,
  with this value. Each ping of a backup moves its alarm 25 hours on; each edit
  of a note moves its indexing five seconds on, with the latest text.
- `update` changes a job that is there and has not started: its value, and its
  time, group or repeat when given. A job that runs or is done is a `conflict`,
  so that a message being sent says it is too late to edit.
- `cancel` takes the job under an id, whatever its state, and says whether
  there was one. A running handler is told to stop: its `signal` aborts in
  TypeScript, its context ends in Go, its task is cancelled in Python, its
  `job.stopped()` turns true in Rust. A repeating job stops repeating.

## Run jobs

```ts
const worker = reminders.work(async job => {
	await push(job.value.userId, job.value.text)
}, { workers: 4 })
// at shutdown
await worker.stop()
```

```rust
let worker = reminders.workers(4).start(|job: &Job<Reminder>| -> Result<(), AppError> {
    push(job.value.user_id, &job.value.text)?;
    Ok(())
})?;
worker.stop()?;   // at shutdown; the store's close stops it too
```

| The handler                 | The job                                                         |
|-----------------------------|-----------------------------------------------------------------|
| returns                     | is done                                                         |
| throws, or returns an error | runs again after its backoff; past `attempts` it fails for good |
| returns `job.retry('10m')`  | runs again in ten minutes, the run counted as an attempt        |
| returns `job.snooze('10m')` | runs again in ten minutes, the run not counted                  |
| returns `job.fail(reason)`  | fails for good now                                              |

- `work` starts `workers` handlers, one unless given, which run each job when
  it is due; one worker is the one order a queue promises. It returns a running
  worker at once, and `stop` lets the handlers under way finish and returns.
- A handler decides what happens to its job by what it returns: `retry`,
  `snooze` and `fail` make the answer to return, and do nothing unless
  returned. In Rust they are `#[must_use]`, and `Ok(())` is done.
- `runDue(handler)` runs what is due when it starts and what falls due
  meanwhile, then returns; it never waits for a later job. Tests, scripts and
  commands use it.
- Every attempt is counted before it runs, so a job that crashes the process
  every time still fails for good after its attempts.
- A run past `timeout` is told to stop and fails. A handler stopped because its
  worker stopped gives its job back, the attempt not counted.
- `queue.claim()` takes the next due job without a handler, for a program that
  settles it itself with `job.done()`, `job.retry()`, `job.snooze()` or
  `job.fail()`. A claimed job not settled within `timeout` goes back to the
  queue, so that it still runs at least once.

## Repeat

```ts
await probes.add({ url }, { id: `probe:${site.id}`, every: '30s' })
await digests.add({ userId: 42 }, { id: 'user:42', cron: '0 20 * * *', timeZone: user.timeZone })

const cleanup = store.schedule('cleanup', { cron: '10 3 * * *', timeZone: 'Europe/Berlin' })
cleanup.work(async job => purge(job.at))
```

```rust
probes.id(&format!("probe:{}", site.id)).every(Duration::from_secs(30)).add(&probe)?;
digests.id("user:42").cron("0 20 * * *", &user.time_zone).add(&Digest { user_id: 42 })?;
let cleanup = store.schedule("cleanup").cron("10 3 * * *", "Europe/Berlin").open()?;
```

- `every: '30s'` runs a job every 30 seconds. Many ids repeating together do
  not run in one instant: each id runs at a phase of its own within the
  interval, the same across restarts.
- `cron` is five fields, minute, hour, day of the month, month and day of the
  week, on the wall clock of `timeZone`, an IANA name, which a cron needs: a
  server's own zone is a guess, and daylight saving's bugs live at 03:10.
  `@hourly`, `@daily`, `@weekly` and `@monthly` are shortcuts.
- A repeating job needs an id, since only an id stops it: `cancel(id)`. It
  moves to its next time after it ran, never overlaps itself, and runs once
  when a program comes back after a night down, not once for every missed
  time. A failed run retries with its backoff, never past the next time.
- A repeat the data owns, one a user, is `add` with `every` or `cron`. A repeat
  the code owns is a schedule: `store.schedule(name, …)` is one repeating job
  under its name, and the code's cron replaces the one kept, each time the
  program opens it. `cleanup.cancel()` removes it. `job.at` is when the run was
  due.

## Limits

```ts
const refreshes = store.queue<Refresh>('refreshes', { concurrency: { total: 8, perGroup: 2 } })
await refreshes.add({ dataset: 3 }, { id: 'refresh:3', group: 'db:42' })
const telegram = store.queue<Message>('telegram', { rate: '30/s' })
```

- `concurrency: 8` runs at most eight jobs of the queue at once, across every
  worker and claim of the store, whatever their `workers`. `perGroup: 2` runs
  at most two of each group: one customer's backlog cannot hold back the
  others. A group bounds how many run; it does not order its jobs.
- `rate: '30/s'` starts at most 30 jobs in any second, as an API's limit asks.
  A rate counts starts, not running jobs, and lives in memory: a restart
  forgets it.

## Where a job is

```ts
const job = await videos.get(id)
// { id, state: 'scheduled' | 'waiting' | 'running' | 'done' | 'failed', at, attempt, ahead, progress, error,
//   lastRun: { startedAt, endedAt, error } }

for await (const job of videos.watch(id)) {   // waiting, 3 ahead … running, 0.4 … done
	send(job.state, job.ahead, job.progress)
}

videos.work(async job => transcode(job.value, done => job.progress(done)))

const page = await later.list({ prefix: `chat:${chatId}:`, limit: 100 })   // { jobs, next }
const failed = await emails.list({ state: 'failed' })                       // the latest failures first
```

- `get` says where a job is: `scheduled` until its time, `waiting` for a worker
  once it is due, `running` while one has it, `failed` once it failed for good,
  `done` while `dedupe` keeps its id; `undefined` for an id with no job. `at` is
  when it runs next. `ahead` counts the jobs that run before a waiting one, up
  to 10,000. `lastRun` is the last run a handler finished: when it started and
  ended, and its error.
- `watch` gives the job as it is, then again each time it changes, and ends
  with it: done, failed, or `cancelled`, a state only a watcher sees.
- `job.progress(value)` shows a running job's progress to `get` and `watch`,
  any JSON up to 4 KiB, a fraction or a count as the application likes. It
  lives in memory: a run a restart ends starts again from nothing.
- `list` reads a page of jobs whose ids start with a prefix, at most `limit`,
  100 unless given, in the byte order of the ids, and `next` asks for the page
  after; `all` walks every page. A prefix is text: `chat:4` meets chat 42 too,
  so end it with a separator.

## Steps

```ts
agents.work(async job => {
	const found = await job.step('search', () => search(job.value.query))
	const answer = await job.step('answer', () => model.answer(found))
	await reply(job.value.chat, answer)
})
```

- `job.step(name, fn)` keeps `fn`'s answer across the job's attempts: the
  attempt after a retry, a lost worker or a restart gets the answer back
  without running `fn` again, and runs only the steps left. A step whose
  attempt ends before its answer is kept runs again, so what `fn` does outside
  the store should bear doing twice.
- A name is a step's within its run, numbered in a loop (`model:1`, `tool:1`);
  an answer is at most 1 MiB of JSON. A run that ends takes its steps with it.

## Transactions

```ts
await db.tx(async tx => {
	tx.exec`update notes set body = ${body} where id = ${id}`
	await tx.with(indexing).add({ noteId: id })      // commits with the row, or not at all
})
```

- A queue opened from an SQL database joins that database's transactions,
  `tx.with(queue).add(…)`, as kv's buckets do. A queue opened from the store
  lives in jobs.db and joins none of the database's.
- `work` and `runDue` inside a transaction are `invalid`.

## Errors

| Error             | When                                                                                                                   | What to do                       |
|-------------------|------------------------------------------------------------------------------------------------------------------------|----------------------------------|
| `invalid`         | a bad name, id, option or repeat; `at` beside `delay`; a value JSON cannot write; a time zone that is not an IANA name | fix the call                     |
| `conflict`        | `update` of a job that runs or is done; a settlement of a claimed job whose time ran out and that another worker took  | read it again                    |
| `limit`           | a value over 1 MiB; a queue at its `maxWaiting`                                                                        | add less, or raise the bound     |
| `closed`          | the store closed                                                                                                       | open it again                    |
| `outcome unknown` | the commit failed after the add ran                                                                                    | `get` the id before adding again |

Every error names the queue and the id: `jobs queue emails: id "welcome:42": conflict`.

## Bounds

| What            | Bound                                                 |
|-----------------|-------------------------------------------------------|
| an id, a group  | 1 to 1024 bytes                                       |
| a value         | 1 MiB of JSON; over 512 bytes it has a row of its own |
| a page          | 1000 jobs or 4 MiB of values                          |
| `ahead`         | counted up to 10,000                                  |
| a step's answer | 1 MiB of JSON                                         |
| progress        | 4 KiB of JSON                                         |
| waiting jobs    | `maxWaiting`, 10,000,000 unless given                 |

## Newcomer check and comparison, 9 October

Two Haiku agents read the first draft with no other context: one said what
each of 33 call sites does, one compared spellings blind. What they found,
and what changed:

| They found                                                                                               | Change                                                                        |
|----------------------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------|
| `drain(handler)` is BullMQ's word for deleting the waiting jobs                                          | `runDue(handler)`                                                             |
| `reschedule(id, value, when)` read as moving the time, the value perhaps ignored                         | `set`: the value and the time, whatever they were, as a key-value store's set |
| `add` that may move a waiting job earlier: neither add nor move                                          | `add` never changes a job that is there, and says whether it added one        |
| `job.retry()` read as if the handler went on after it, and two decisions could clash                     | the handler returns its decision: `return job.snooze('10m')`                  |
| `next()` read as a peek or an iterator                                                                   | `claim()`                                                                     |
| `keep: { done }` both keeps records and blocks adding: shortening it to save space would run a job twice | `dedupe` for the ids, `keep` for failed jobs                                  |
| `zone` read as an availability zone                                                                      | `timeZone`                                                                    |
| `backoff: { from, to }` read as a random range                                                           | `backoff: { initial, max }`                                                   |
| `capacity` read as a count of workers                                                                    | `maxWaiting`                                                                  |
| a job due later shown as `waiting`                                                                       | `scheduled` until its time, `waiting` once due                                |
| a schedule's cron changed in the code that never reached the job kept                                    | the code's cron replaces the kept one when the program opens it               |
| `queue.stop()` stopping every loop of a queue                                                            | `work` returns its worker, which `stop` stops                                 |
| `index` read as a search index                                                                           | the example's queue is `indexing`                                             |

Read right at once: `add`, `id`, `delay`, `at`, `every`, `cron`,
`concurrency`, `rate`, `get`, `watch`, `list`, `lastRun`. `step` was read as
a log entry by one; its first line now says it keeps an answer across
attempts, as Inngest's `step.run` does.

## Was, in Go

| Was                                             | Now                                                                  |
|-------------------------------------------------|----------------------------------------------------------------------|
| `jobs.Open`, `OpenQueue[T](queues, …)`          | `store.queue<T>(…)`; the engine opens with the first queue           |
| `Enqueue`                                       | `add`, which says whether it added                                   |
| `Key`                                           | `id`                                                                 |
| `After`, `At`                                   | `delay`, `at`                                                        |
| `Move()`                                        | `set`                                                                |
| `Every(d, Spread())`, `Every(d)`                | `every`, spread by id always                                         |
| `Cron`, `Daily` with a `*time.Location`         | `cron` with a `timeZone`; `@daily` and the rest as shortcuts         |
| `OpenSchedule`                                  | `store.schedule`                                                     |
| `MaxRunning`, `MaxRunningInGroup`, `Workers`    | `concurrency: { total, perGroup }`; `workers` on `work`              |
| `Rate(n, per)`                                  | `rate: '30/s'`                                                       |
| `MaxAttempts`, `Backoff(first, most)`           | `attempts`, `backoff: { initial, max }`                              |
| `KeepDone`, `KeepFailed`                        | `dedupe`, `keep`                                                     |
| `MaxWaiting`                                    | `maxWaiting`                                                         |
| `Retry`, `Snooze`, `Fail` called on the job     | returned by the handler                                              |
| `Lease`, `Claim`, `Ack`, `Extend`               | inside; `claim()` and `job.done()` for a program that settles itself |
| `UntilIdle()`                                   | `runDue`                                                             |
| `Entry.Ran`, `Entry.Took` in milliseconds       | `lastRun.startedAt`, `lastRun.endedAt`                               |
| `Scan`, `Query{Prefix, State}`                  | `list({ prefix, state })`, `all`                                     |
| `Options.In`, `Enqueued`                        | `db.queue(…)`, `tx.with(queue).add`                                  |
| `jobs.Step(ctx, job, name, fn)`, `Kept`, `Keep` | `job.step(name, fn)`                                                 |
