# jobs: the API book

Accepted by the owner, 9 October 2026, in its second version: reworked after
a survey of the job libraries people use today and a second round of checks
by readers who had seen nothing of TinyStore (below). Work an application must do later, or now
but outside the request that asked for it: a reminder at six, an email in the
background, a push to every member of a group, an account deleted thirty days
after its owner asked, a cleanup every night. This book is the API before the
code; [dx.md](../dx.md) has the rules it follows, and [kv.md](kv.md) the words
both books share. TypeScript comes first; Python, Go and Rust follow where
they spell something differently. The Rust column is built
(`crates/tinystore/src/jobs`), all but `watch`, transactions, and queues
opened from an SQL database, which come with the wire and with sqldb.

## What a newcomer learns

| Concept                     | In one line                                                                         |
|-----------------------------|-------------------------------------------------------------------------------------|
| queue                       | jobs of one type, run in the order of their time                                    |
| `add`                       | puts a job in a queue: it runs now, after a `delay`, or `at` a time                 |
| `work`                      | runs a handler on each job: it returns, the job is done; it throws, it runs again   |
| `id`                        | a name the caller gives a job, to find it, change it, cancel it, or add it once     |
| `set`, `update`, `cancel`   | an id's job made this whatever it was, changed while it waits, taken away           |
| `every`, `cron`, `schedule` | a job that repeats: an interval or the wall clock; a schedule is the code's own one |
| `attempts`, `backoff`       | how many runs a job gets before it fails for good, and the wait between them        |
| `concurrency`, `rate`       | how many run at once, in all and in each group; how many start in a span            |

The first three are all most programs need:

```ts
const emails = store.queue<Email>('emails')
await emails.add({ to, subject, body })
emails.work(async email => {
	await smtp.send(email)
})
```

A job runs at least once: a process that dies after the work and before the
job was marked done runs it again. A handler keys its effect by what the job
names, `insert … on conflict do nothing`, so that twice is harmless.

## Open a queue

```ts
const reminders = store.queue<Reminder>('reminders')
const emails = store.queue<Email>('emails', { attempts: 20, backoff: { initial: '5s', max: '30m' } })
const refreshes = store.queue<Refresh>('refreshes', { concurrency: { total: 8, group: 2 } })
const telegram = store.queue<Message>('telegram', { rate: '30/s' })
const pushes = store.queue<Push>('pushes', { dedupe: '1h' })
```

```python
reminders = store.queue("reminders", Reminder)
emails = store.queue("emails", Email, attempts=20, backoff=Backoff(initial="5s", max="30m"))
refreshes = store.queue("refreshes", Refresh, concurrency=Concurrency(total=8, group=2))
```

```go
reminders, err := jobs.Queue[Reminder](store, "reminders")
emails, err := jobs.Queue[Email](store, "emails", jobs.Attempts(20), jobs.Backoff{Initial: 5 * time.Second, Max: 30 * time.Minute})
refreshes, err := jobs.Queue[Refresh](store, "refreshes", jobs.Concurrency{Total: 8, Group: 2})
```

```rust
let reminders = store.queue::<Reminder>("reminders").open()?;
let emails = store.queue::<Email>("emails").attempts(20).backoff(Duration::from_secs(5), Duration::from_mins(30)).open()?;
let refreshes = store.queue::<Refresh>("refreshes").concurrency(Concurrency::total(8).group(2)).open()?;
let telegram = store.queue::<Message>("telegram").rate(30, Duration::from_secs(1)).open()?;
```

| Option        | Default                        | What it does                                                                                            |
|---------------|--------------------------------|---------------------------------------------------------------------------------------------------------|
| `attempts`    | 10                             | runs a job gets before it fails for good, the first run counted                                         |
| `backoff`     | `{ initial: '1s', max: '1h' }` | the wait before a retry: `initial`, doubling each time up to `max`, a tenth longer or shorter at random |
| `timeout`     | `'1m'`                         | how long one run may take before it is told to stop and fails                                           |
| `concurrency` | one at a time in each worker   | jobs running at once across every worker of the store: `8`, or `{ total: 8, group: 2 }`                 |
| `rate`        | none                           | jobs started in a span: `'30/s'`                                                                        |
| `dedupe`      | none                           | how long a done job's id makes `add` add nothing                                                        |
| `keep`        | `'7d'`                         | how long a failed job stays, with its error, to be found and started again                              |
| `maxWaiting`  | 10,000,000                     | jobs that may wait; an `add` past it is `limit`                                                         |

A value is JSON, so a worker in another language reads it. A job waits
across deploys, and the type that added it may not be the type that reads
it: in TypeScript a [Standard Schema](https://standardschema.dev),
`store.queue('emails', { schema: Email })` with zod, valibot or arktype, gives
the queue its type and checks each value before a handler gets it, as
Python's type and Rust's serde do. A value that no longer reads fails its job
for good, not its queue.

A queue opened from an SQL database, `db.queue('indexing')`, keeps its jobs in
that database's file, so that a job commits with the rows it is about.

## Add a job

```ts
await reminders.add({ userId: 42, text: 'Call mom' })                       // now
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
- Adds from many callers share a commit, so a loop of them, or a
  `Promise.all`, costs a commit a group rather than a commit a job.
- In Rust, a job's options are a chain that ends in its verb, as kv's `key()`
  is: `queue.id(…)`, `.at(…)` or `.delay(…)`, `.group(…)`, `.every(…)` or
  `.cron(…)`, then `.add(&value)`.

## An id

An `id` names one job of a queue, 1 to 1024 bytes of text. Most jobs need
none; a job gets one to be found, changed, cancelled, or added once however
many times its caller asks. An id is taken while its job is scheduled,
waiting, running or failed, and while `dedupe` keeps it once the job is done.
Four calls take it:

| The id's job is                        | `add`            | `set`                                        | `update`                                           | `cancel`                          |
|----------------------------------------|------------------|----------------------------------------------|----------------------------------------------------|-----------------------------------|
| none                                   | adds it: `true`  | adds it                                      | nothing: `false`                                   | `false`                           |
| `scheduled`, or `waiting` for a worker | nothing: `false` | replaces its value and time                  | changes its value, and its time when given: `true` | removes it: `true`                |
| `running`                              | nothing: `false` | one run more after this one, with this value | nothing: `false`                                   | tells its handler to stop: `true` |
| `failed`, kept for `keep`              | nothing: `false` | starts it again, its attempts from one       | nothing: `false`                                   | removes it: `true`                |
| `done`, while `dedupe` keeps its id    | nothing: `false` | adds it again                                | nothing: `false`                                   | `false`                           |

```ts
const added = await later.add(draft, { id, at: nineAm })           // false: a second tap on "send"
await checks.set(`check:${check.id}`, check, { delay: '25h' })      // each ping moves the alarm 25 h on
const edited = await later.update(id, draft, { at: tenAm })        // false: it is being sent, or was
const cancelled = await later.cancel(id)                            // false: it ran already
```

```rust
checks.id(&format!("check:{}", check.id)).delay(Duration::from_hours(25)).set(&check)?;
let edited: bool = later.id(&id).at(ten_am).update(&draft)?;
let cancelled: bool = later.cancel(&id)?;
```

- `add` adds a job only to an id that is free, as kv's `create` writes only a
  key that is not there: a user who taps "send" twice adds one job, a job
  running or failed is left as it is, and a failure stays to be looked at.
- `set` makes the id's job this value at this time, whatever it was: a
  deadline that moves, a value that must be the latest, a failed job started
  again. A job running when `set` comes runs once more after it, with the new
  value, since the run under way may have read what changed.
- `update` changes a job that has not started: its value, and its time, group
  or repeat when the call gives them. It answers `false` for one that runs,
  ran or never was, as `add` and `cancel` answer rather than throw: a message
  being sent says it is too late to edit.
- A call answers what it did by the id's state when it ran; two calls at once
  on one id run one after the other, the second seeing what the first did.
- `cancel` takes the job under an id and says whether there was one. A running
  handler is told to stop: its `run.signal` aborts in TypeScript, its context
  ends in Go, its task is cancelled in Python, its `run.stopped()` turns true in
  Rust. What it did before it stopped stays done. A repeating job stops
  repeating.
- With `dedupe: '1h'` an id runs once in an hour: a done job keeps its id
  taken for the hour, and `add` of it adds nothing. `set` arms it again.

Two patterns people look for by name are these calls with a delay:

| Pattern  | Call                                                             | What happens                                                       |
|----------|------------------------------------------------------------------|--------------------------------------------------------------------|
| debounce | ``indexing.set(`note:${id}`, { noteId: id }, { delay: '5s' })``  | each edit moves the job five seconds on, with the latest value     |
| throttle | ``reports.add({ team }, { id: `report:${team}`, delay: '1m' })`` | the first call's time and value stand; calls meanwhile add nothing |

## Run jobs

```ts
const worker = reminders.work(async ({ userId, text }) => {
	await push(userId, text)
})
// at shutdown
await worker.stop()
```

```ts
pushes.work(async (push, run) => {
	const answer = await provider.send(push)
	if (answer.unknownToken) return run.fail('the provider no longer knows this token')
	if (answer.retryAt) return run.snooze(answer.retryAt)
})
```

```rust
let worker = reminders.work(|reminder: Reminder, _run| push(reminder.user_id, &reminder.text))?;
worker.stop();   // at shutdown; the store's close stops it too

pushes.work(|push: Push, run| match provider.send(&push) {
    Err(SendError::UnknownToken) => Ok(run.fail("the provider no longer knows this token")),
    Err(SendError::SlowDown(wait)) => Ok(run.snooze(wait)),
    Err(other) => Err(other),
    Ok(()) => Ok(run.done()),
})?;
```

| The handler                 | The job                                                         |
|-----------------------------|-----------------------------------------------------------------|
| returns                     | is done                                                         |
| throws, or returns an error | runs again after its backoff; past `attempts` it fails for good |
| returns `run.retry('10m')`  | runs again in ten minutes, the run counted as an attempt        |
| returns `run.snooze('10m')` | runs again in ten minutes, the run not counted                  |
| returns `run.fail(reason)`  | fails for good now                                              |

- A handler gets the job's value first and the run second. The run is this
  call on the job: `run.id`, `run.at`, when it was due, `run.attempt`,
  `run.signal`, `run.step()`, `run.setProgress()`, and the three answers.
- `retry`, `snooze` and `fail` make the answer to return. An answer made and
  not returned fails the run with an error that says so, since a snooze lost
  for a missing `return` would otherwise mark the job done. A time for `retry`
  or `snooze` is a span or a `Date`. In Rust they are `#[must_use]` beside
  `run.done()`, and a handler that never answers otherwise returns `Ok(())`;
  an `Err` retries, its text kept as the job's error.
- A snooze counts no attempt, so a handler that snoozes for ever runs for
  ever, as River's and Oban's do. `run.attempt` is this run's number among the
  job's attempts, the first being 1.
- `work` runs as many handlers at once as the queue's `concurrency` lets, one
  when it sets none, which is the one order a queue promises;
  `work(handler, { concurrency: 4 })` runs at most four in this worker, under
  the queue's bound. It returns a running worker at once. `stop` takes no job
  more and waits for the handlers under way, each bounded by its `timeout`;
  `store.close()` stops every worker so.
- Every attempt is counted before it runs, so a job that crashes the process
  every time still fails for good after its attempts.
- A run past `timeout` is told to stop: its signal aborts, `run.stopped()`
  turns true, and what the handler returns then settles the job as ever. A
  worker that stops gives back the jobs it held for busy handlers, their
  attempts not counted, and waits for the handlers under way.
- `runDue(handler)` runs what is due when it starts and what falls due
  meanwhile, then returns; it never waits for a later job. Tests, scripts and
  commands use it.

## Repeat

```ts
await probes.set(`probe:${site.id}`, { url: site.url }, { every: '30s' })
await digests.set(`user:${user.id}`, { userId: user.id }, { cron: '0 20 * * *', timeZone: user.timeZone })
await digests.cancel(`user:${user.id}`)

const cleanup = store.schedule('cleanup', { cron: '10 3 * * *', timeZone: 'Europe/Berlin' }, async run => {
	await purgeDeletedBefore(daysBefore(run.at, 30))
})
store.schedule('sync', { every: '15m' }, syncInvoices)

const last = await cleanup.get()   // { at: the next run, lastRun, error }: what a page of cron jobs shows
```

```rust
probes.id(&format!("probe:{}", site.id)).every(Duration::from_secs(30)).set(&probe)?;
digests.id("user:42").cron("0 20 * * *", &user.time_zone).set(&Digest { user_id: 42 })?;
let cleanup = store.schedule("cleanup").cron("10 3 * * *", "Europe/Berlin").work(|run| purge(run.at()))?;
```

- `every: '30s'` runs a job every 30 seconds. Many ids repeating together do
  not run in one instant: each id runs at a phase of its own within the
  interval, the same across restarts, its first run within the next interval;
  `at` or `delay` beside a repeat sets its first run instead. `every` is for
  work whose moment does not matter; the wall clock is `cron`'s.
- `cron` is five fields, minute, hour, day of the month, month and day of the
  week, or `@hourly`, `@daily`, `@weekly` and `@monthly`, and it needs a
  `timeZone`, an IANA name, `'UTC'` among them: an hour means nothing without a
  zone, and a default would be a guess. A `timeZone` given as `undefined`, a
  user whose zone was never set, is `invalid` rather than UTC. A time daylight
  saving skips runs at the first minute after it, and one it repeats runs once.
- A repeat needs an id, since only an id stops it. It moves to its next time
  after it ran, never overlaps itself, and runs once when a program comes back
  after a night down, not once for every missed time. A failed run retries with
  its backoff, never past the next time, and the repeat goes on.
- A repeat the data owns, a user's digest, is `set` with `every` or `cron`,
  and `cancel` ends it. A repeat the code owns is a schedule:
  `store.schedule(name, when, handler)` is one repeating job under its name
  and the handler that runs it, started at once, as `work` is. The code's
  repeat replaces the one kept each time the program opens it, so a schedule
  changed in the code is the one that runs. Its handler gets the run alone,
  there being no value; `run.at` is when it was due, which "thirty days ago" is
  counted from even when the run starts late. `stop`, `get` and `runDue` work
  on a schedule as on a worker and a queue.

## Limits

```ts
const refreshes = store.queue<Refresh>('refreshes', { concurrency: { total: 8, group: 2 } })
await refreshes.add({ dataset: 3 }, { id: 'refresh:3', group: 'db:42' })
const telegram = store.queue<Message>('telegram', { rate: '30/s' })
```

- `concurrency: 8` runs at most eight jobs of the queue at once, across every
  worker of the store, whatever each worker's own. `group: 2` runs at most two of
  each group: one customer's backlog cannot hold back the others. `group: 1`
  runs a group's jobs one at a time, in their order. A job added without a
  `group` is bound by the total alone.
- `rate: '30/s'` starts at most 30 jobs in any second, as an API's limit asks.
  A rate counts starts, retries among them, not running jobs, and lives in
  memory: a restart forgets it.

## Where a job is

```ts
const job = await videos.get(id)
// { id, value, state, at, attempt, ahead, progress, error, group, repeat, lastRun: { startedAt, endedAt } } | undefined

for await (const job of videos.watch(id)) {   // waiting, 3 ahead … running, 0.4 … done
	send(job.state, job.ahead, job.progress)
}

videos.work(async (video, run) => transcode(video, done => run.setProgress(done)))

const page = await later.list({ prefix: `chat:${chatId}:`, limit: 100 })   // { jobs, next }
const failed = await emails.list({ state: 'failed' })                       // the latest failures first
```

- `get` says where a job is: `scheduled` until its time, `waiting` for a worker
  once it is due, `running` while one has it, `failed` once it failed for good,
  `done` while `dedupe` keeps its id; `undefined` for an id with no job. `at` is
  when it runs next. `ahead` counts the jobs that run before a waiting one, up
  to 10,000. `error` is why its last run failed; `lastRun` is when the last run
  a handler finished started and ended.
- `watch` gives the job as it is, then again each time it changes, and ends
  with it: done, failed, or `cancelled`, a state only a watcher sees.
- `run.setProgress(value)` shows a running job's `progress` to `get` and `watch`,
  any JSON up to 4 KiB, a fraction or a count as the application likes. It
  lives in memory: a run a restart ends starts again from nothing.
- `list` reads a page of jobs whose ids start with a prefix, at most `limit`,
  100 unless given, in the byte order of the ids, and `next` asks for the page
  after; `all` walks every page. A prefix is text: `chat:4` meets chat 42 too,
  so end it with a separator.

## Steps

```ts
agents.work(async (question, run) => {
	const found = await run.step('search', () => search(question.text))
	const answer = await run.step('answer', () => model.answer(question.text, found))
	await reply(question.chat, answer)
})
```

- `run.step(name, fn)` keeps `fn`'s answer across the job's attempts: the
  attempt after a retry, a lost worker or a restart gets the answer back
  without running `fn` again, and runs only the steps left. A step whose
  attempt ends before its answer is kept runs again, so what `fn` does outside
  the store should bear doing twice.
- A name is a step's within its job, numbered in a loop (`model:1`, `tool:1`);
  an answer is at most 1 MiB of JSON. A job that ends, done, failed or
  cancelled, takes its steps with it, and a repeat's next time starts without
  them.

## Transactions

```ts
await db.tx(async tx => {
	await tx.exec`update notes set body = ${body} where id = ${id}`
	await tx.with(indexing).add({ noteId: id })      // commits with the row, or not at all
})

await store.tx(async tx => {
	for (const member of members) await tx.with(pushes).add({ member, message }, { id: `${message}:${member}` })
})
```

- A queue opened from an SQL database joins that database's transactions,
  `tx.with(queue).add(…)`, as kv's buckets do. A queue opened from the store
  lives in jobs.db, and `store.tx` commits its jobs together, or none of them;
  a bucket and a queue of the store are two files, and one transaction takes
  the handles of one file.
- `work` and `runDue` inside a transaction are `invalid`.

## Tests

```ts
const store = await open(dir, { private: true, clock: new Date('2026-10-09T09:00:00Z') })
const reminders = store.queue<Reminder>('reminders')
await reminders.add(reminder, { delay: '1h' })
await reminders.runDue(remind)          // nothing is due yet
await store.clock.advance('1h')
await reminders.runDue(remind)          // runs it
```

A store opened with `background: false` starts no work of its own, and a test
moves its clock and runs what is due with `runDue`.

## Errors

| Error             | When                                                                                                                                                    | What to do                       |
|-------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------|----------------------------------|
| `invalid`         | a bad name, id, option or repeat; `at` beside `delay`; a cron without a time zone, or with one unknown; a value JSON cannot write or its schema refuses | fix the call                     |
| `conflict`        | Rust's settlement of a claimed job whose time ran out and that another worker took                                                                      | let it go                        |
| `limit`           | a value over 1 MiB; a queue at its `maxWaiting`                                                                                                         | add less, or raise the bound     |
| `closed`          | the store closed                                                                                                                                        | open it again                    |
| `outcome unknown` | the commit failed after the add ran                                                                                                                     | `get` the id before adding again |

Every error names the queue and the id: `jobs queue digests: id "user:42": time zone "Europe/Berlinn": invalid`.

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

## Rust

- Every call blocks its thread, as kv's do. `work` returns at once: its
  handlers run on threads the store starts for it and stops when it closes,
  so the store still owns every thread that outlives a call. A `Worker`
  dropped keeps running until the store closes; `stop` stops it and waits.
- A handler is `Fn(V, &Run) -> Result<R, E>`, where `R` is `()` or an
  `Outcome` and `E` anything that displays. A panic is an error with its
  message.
- `run.done()`, `run.retry(when)`, `run.snooze(when)` and `run.fail(reason)`
  make an `Outcome`, a `when` being a `Duration` or a `SystemTime`.
- `queue.concurrency(4).work(handler)` bounds this worker as
  `work(handler, { concurrency: 4 })` does.
- `queue.claim()` takes the next due job for a loop of the program's own, and
  `claimed.settle(outcome)` settles it; a job not settled within `timeout`
  goes back to the queue. The other languages have no `claim`: their `work`
  is the engine's loop, over the pipe or the network.
- An async program runs an async handler through its runtime's handle,
  `handle.block_on(…)`, from the worker's own thread. Whether the Rust API
  grows `async` calls is an open decision.

## Python and Go

```python
await later.add(draft, id=f"chat:{chat_id}:{draft.id}", at=nine_am)
await checks.set(f"check:{check.id}", check, delay="25h")


async def remind(reminder: Reminder, run: tinystore.Run) -> None:
    await push(reminder.user_id, reminder.text)


reminders.work(remind, concurrency=4)
store.schedule("cleanup", purge, cron="10 3 * * *", time_zone="Europe/Berlin")
```

```go
added, err := later.Add(ctx, draft, jobs.ID(key), jobs.At(nineAM))
err = checks.Set(ctx, key, check, jobs.Delay(25*time.Hour))
err = reminders.Work(ctx, func(ctx context.Context, r Reminder, run *jobs.Run) error {
	if busy {
		return run.Snooze(time.Minute)
	}
	return push(ctx, r.UserID, r.Text)
}, jobs.Concurrency(4))
```

Go's `Work` blocks until its context ends, as a Go loop does; an answer is
an error value the handler returns, as River's are.

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

## Survey, 9 October

Three helper agents read how the libraries in use spell the same things, from
their documentation and sources: BullMQ, pg-boss, graphile-worker,
Trigger.dev, Inngest, Hatchet, DBOS, Absurd and the queues of Cloudflare, Deno
and Vercel; Celery, Dramatiq, RQ, Huey, arq, Procrastinate, Sidekiq, Solid
Queue, GoodJob and Oban; River, Asynq, apalis, underway, fang, sqlxmq,
effectum and the other queues built on SQLite. What the book takes from them,
and where it goes its own way:

- **The value first.** Celery, Dramatiq, Sidekiq, Hatchet, Trigger.dev, Absurd
  and graphile-worker hand a handler its payload, with the job's state beside
  it; BullMQ, River and Oban hand a job whose payload is a field.
- **An answer returned.** Oban's `{:snooze, …}` and `{:cancel, …}`, River's
  `JobSnooze` and `JobCancel`, awa's `JobResult`: a handler returns its
  decision, and a snooze counts no attempt in Oban, River and Huey.
- **`step`** is the word of Inngest, Cloudflare, Absurd, DBOS and, since 8.1,
  Rails' continuable jobs.
- **A limit for each group** is a key the job carries: Solid Queue's
  `limits_concurrency key:`, DBOS's partitions, Hatchet's concurrency
  expressions, and the partitions of River's and Oban's paid editions.
- **An id added twice.** BullMQ ignores a repeated `jobId`, Trigger.dev
  answers with the first run, DBOS throws, and graphile-worker replaces unless
  told otherwise. Here `add` answers `false` and `set` replaces, so the call
  says which the caller meant.
- **A cron's zone.** Celery, Huey, Oban, Hatchet, Asynq, River, pg-boss and
  Trigger.dev run a cron in UTC unless told, graphile-worker and `Deno.cron`
  in UTC alone; Sidekiq, GoodJob, RQ, arq and DBOS's TypeScript SDK take the
  host's zone; underway and Inngest take it in the expression,
  `"0 2 * * *[UTC]"`, `"TZ=Europe/Paris 0 12 * * 5"`. The book asks for it
  beside the expression: a zone required can stop being required later
  without breaking a caller, and a default cannot become a requirement.
- **A worker kept running.** River's `Start` returns and `Stop` stops; an
  effectum worker stops when its handle is dropped, and its documentation
  warns to keep it. Here a worker nobody holds runs until the store closes, in
  Rust as in TypeScript.
- **Words left to others.** `drain` deletes in BullMQ and runs in Oban and
  Sidekiq; `lock` is a queue by key in Procrastinate, beside a `queueing_lock`
  that deduplicates; `discard` is ActiveJob's failure; `revoke` is Celery's
  cancel.
- **Asked for elsewhere, later here:** a rate for each key (Hatchet, Inngest,
  DBOS, Sidekiq Enterprise), pausing a queue or a schedule (BullMQ, Oban, DBOS,
  Cloudflare Workflows), schedules listed and removed at run time (DBOS,
  Hatchet, pg-boss, Solid Queue), a hook once a job has failed for good
  (Sidekiq's `sidekiq_retries_exhausted`), and a cap on how long a debounce's
  calls may push its run on (Trigger.dev's `maxDelay`; `set` pushes for as long
  as calls come, which a dead man's switch needs).

## Second round, 9 October

Three Haiku agents read the second draft with no other context: one said
what each of 36 call sites does, one compared seventeen spellings blind, one
reviewed the API as a developer choosing a library.

| They found                                                                                    | Change                                                                                                                 |
|-----------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------|
| `job.value` wherever a handler reads its job, though most handlers need the value alone       | the handler gets the value first: `work(async ({ userId, text }, run) => …)`                                           |
| `job` naming both the handler's argument and the record `get` returns                         | the argument is `run`, this call on the job; `get` returns a `Job`                                                     |
| `job.progress(0.4)` read as a getter beside the `progress` a watcher reads                    | `run.setProgress(0.4)`                                                                                                 |
| `add` of a running id running the job twice, and of a failed id wiping the failure kept       | `add` writes only a free id, as kv's `create` does; `set` runs a running job once more and starts a failed one again   |
| `update` throwing `conflict` on a race its caller cannot avoid                                | `update` answers `false`, as `add` and `cancel` do                                                                     |
| `update(id, { value, at })` beside `set(id, value, options)`                                  | `update(id, value, options)`, the shape `set` has                                                                      |
| `workers` beside `concurrency`, both read as how many run at once                             | `concurrency` alone: the queue's across the store, `work`'s in one worker, which runs the queue's total unless it says |
| a schedule opened, then given its handler                                                     | `store.schedule(name, when, handler)`, one call                                                                        |
| `Outcome::snooze(d)` in Rust beside `run.snooze('1m')` in TypeScript                          | `run.snooze(d)` and `run.done()` in Rust too                                                                           |
| a `return` forgotten before `run.snooze()` marking the job done                               | an answer made and not returned fails the run, saying so                                                               |
| `timeZone: user.timeZone` running in UTC for a user without one; a cron's hour without a zone | a cron needs a `timeZone`, and `undefined` is `invalid`                                                                |
| a schema or options as a queue's second argument                                              | `{ schema }` beside the other options                                                                                  |

Kept after the blind comparison: `set` over `schedule`, `replace` and
`upsert`; `step` over `checkpoint`, `once` and `remember`; `runDue` over
`drain`; ids flat on the queue rather than `queue.job(id)`; `Job` for the
record `get` returns.

The owner kept `dedupe`, which the blind reader would spell
`idempotencyWindow`, `step`, which the newcomer check would spell
`checkpoint`, and a cron's required `timeZone`, which most libraries replace
with UTC, and accepted the book. Asked for and not in the first version: a
hook for a job that failed for good, which channels will carry; a `rate` for
each group, as Telegram's message a second a chat asks; pausing a queue;
cancelling by prefix; a stable identity for a job added without an id, to key
an outside effect by.

## Was, in Go

| Was                                             | Now                                                          |
|-------------------------------------------------|--------------------------------------------------------------|
| `jobs.Open`, `OpenQueue[T](queues, …)`          | `store.queue<T>(…)`; the engine opens with the first queue   |
| `Enqueue`                                       | `add`, which says whether it added                           |
| `Key`                                           | `id`                                                         |
| `After`, `At`                                   | `delay`, `at`                                                |
| `Move()`                                        | `set`                                                        |
| `Every(d, Spread())`, `Every(d)`                | `every`, spread by id always                                 |
| `Cron`, `Daily` with a `*time.Location`         | `cron` with a `timeZone`; `@daily` and the rest as shortcuts |
| `OpenSchedule`                                  | `store.schedule`                                             |
| `MaxRunning`, `MaxRunningInGroup`, `Workers`    | `concurrency: { total, group }`; `concurrency` on `work`     |
| `Rate(n, per)`                                  | `rate: '30/s'`                                               |
| `MaxAttempts`, `Backoff(first, most)`           | `attempts`, `backoff: { initial, max }`                      |
| `KeepDone`, `KeepFailed`                        | `dedupe`, `keep`                                             |
| `MaxWaiting`                                    | `maxWaiting`                                                 |
| `Retry`, `Snooze`, `Fail` called on the job     | returned by the handler                                      |
| `Lease`, `Claim`, `Ack`, `Extend`               | inside; Rust's `claim()` and `settle` for a loop of its own  |
| `UntilIdle()`                                   | `runDue`                                                     |
| `Entry.Ran`, `Entry.Took` in milliseconds       | `lastRun.startedAt`, `lastRun.endedAt`                       |
| `Scan`, `Query{Prefix, State}`                  | `list({ prefix, state })`, `all`                             |
| `Options.In`, `Enqueued`                        | `db.queue(…)`, `tx.with(queue).add`                          |
| `jobs.Step(ctx, job, name, fn)`, `Kept`, `Keep` | `run.step(name, fn)`                                         |
