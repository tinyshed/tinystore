# The API's vocabulary

The rewrite is the one chance to fix the names, because every name changes
anyway. The vocabulary is decided before the code, one engine at a time, in
an API book the owner accepts.

## What is wrong today

An inventory of `e81a050`: 827 exported names in Go (`tinystore` 70,
`metrics` 178, `records` 120, `records/console` 21, `sqldb` 90, `kv` 148,
`jobs` 112, `blobs` 76, `backup` 12), 135 top-level exports and 209 class
members in Bun, 72 names and 173 class members in Python.

| Word     | What it means today                                                                                                  |
|----------|----------------------------------------------------------------------------------------------------------------------|
| `Batch`  | four things: sqldb's group commit, metrics' ingest input, records' follow result, and Bun's and Python's transaction |
| `Query`  | four structs with four sets of fields: records, kv, jobs, blobs                                                      |
| `Store`  | the directory and every engine's handle; sqldb calls its handle `DB`                                                 |
| `Every`  | a background loop on the store and a repeat in jobs                                                                  |
| `Rate`   | kv's limiter, jobs' starts, and quota windows written `'100/5h'`                                                     |
| `Claim`  | reserving a file for an engine, and leasing a job                                                                    |
| `Keep`   | keeping finished jobs, and keeping a step's answer                                                                   |
| `Change` | three identical interfaces in kv, jobs and sqldb                                                                     |
| `All`    | an iterator in kv, jobs, blobs and records, a slice in sqldb                                                         |

Bare numbers mean different units: `every: 30` is 30 ms in Bun and 30 s in
Python; `took` and `retryAfter` are milliseconds in Bun and seconds in Python;
Python reads an integer time as nanoseconds in records and milliseconds in
metrics.

The worst names, the ones a newcomer misreads first: `move: true`,
`maxRunningInGroup`, `spread: true`, `after` (Go's `time.After` returns a
channel), `key` (identity and deduplication at once), `keepDone`,
`untilIdle`, `loseAtMost`, `sliding`, `Written`, `Enqueued`, `In`, `Guest`,
`Manual`, `SelfMetrics`, `Readers` beside `MaxReaders`, `Lookback`.

## Rules

- **One concept, one word,** in every engine and every language.
- **A name is at most two words.** A third word means the concept wants a
  nested option or a verb of its own.
- **A flag never changes what a call does.** Another behaviour is another verb
  (`set`, `runDue`), an enumeration, or another constructor. A boolean
  only turns a feature on or off.
- **Time is typed:** text such as `'30s'`, `Duration`, `timedelta`, `Date`,
  `datetime`. A bare number is refused, as input and as output.
- **Storage words stay inside:** head, segment, block, seal, merge, lease,
  lookback, watermark. `claim` is a job's, for a program that settles it
  itself.
- **Take the word people already know:** BullMQ (`add`, `delay`,
  `concurrency`), Redis (`ttl`, `publish`), S3 (`ifMatch`), Prometheus
  (`rate`, `increase`), `tracing` and `slog`.
- **What engine authors use lives in a module of its own,** away from the
  application's names.
- **Nesting is welcome when it gathers related settings under one known word
  and has a short form:** `concurrency: 8` or
  `concurrency: { total: 8, group: 2 }`.
- **Names on the store say what a thing is, not which engine keeps it:**
  `store.bucket`, `store.counters`, `store.queue`, never `store.kv.bucket`.
  Where it lives is said by what opened it: the store, or an SQL database
  (`db.bucket`). Go is the exception its language makes, since a Go method
  takes no type parameter: `kv.Bucket[T](store, …)`.

## The newcomer check

A helper agent that has read nothing about TinyStore gets one call site from
a book, a line or a few, and says in one sentence what it does and what it
returns. The book keeps the answer beside the call. A wrong or hesitant answer
means a rename, never a comment. A book is accepted when every call site in it
passes and the owner agrees.

## Words

Decided words are settled; draft words wait for their engine's book.

| Word                               | Means                                                                             | Where                  | Was                                                 | State   |
|------------------------------------|-----------------------------------------------------------------------------------|------------------------|-----------------------------------------------------|---------|
| `channels`, `publish`, `subscribe` | messages between parts of an application                                          | runtime                | signals, an idea                                    | decided |
| `durability`                       | a file's mode: `'full'` or `'os'`                                                 | every engine           | FULL only                                           | decided |
| `flushEvery`                       | kept in memory and written every span                                             | kv counters            | `loseAtMost`, `durability: '1s'`                    | draft   |
| `concurrency`                      | how many run at once: `8` or `{ total, group }`                                   | jobs                   | `maxRunning`, `maxRunningInGroup`, `workers`        | decided |
| `rate`                             | starts or calls a span, as text `'30/s'`                                          | jobs, kv limiter       | `Rate(n, per)` in Go                                | decided |
| `peek`, `reset`                    | a limit's answer without using it; forgetting a key                               | kv limits              | `Get`, `Delete`, `check`                            | draft   |
| `keep`                             | how long failed or old things stay                                                | jobs, records, metrics | `keepFailed`, `Retention`                           | draft   |
| `dedupe`                           | how long a done job's id makes adding it again add nothing                        | jobs                   | `keepDone`                                          | draft   |
| `ttl`                              | when a key expires                                                                | kv                     | the same                                            | draft   |
| `idle`                             | expires after a span without reads or writes                                      | kv                     | `sliding`                                           | draft   |
| `under`                            | a branch: whose keys these are, cleared in one call                               | kv                     | `Of`, `of`                                          | draft   |
| `id`                               | a job's identity; adding an id that is taken adds nothing                         | jobs                   | `key`                                               | draft   |
| `group`                            | jobs that share a group's `concurrency`                                           | jobs                   | the same                                            | draft   |
| `add`                              | puts a job in a queue                                                             | jobs                   | `enqueue`                                           | draft   |
| `delay`, `at`                      | when a job runs: after a span, at a time                                          | jobs                   | `after`, `at`                                       | draft   |
| `every`, `cron`                    | repeats: an interval spread by id, or the wall clock of a time zone               | jobs                   | `every` with `spread: true`                         | draft   |
| `set`                              | makes an id's job this value at this time, whatever it was                        | jobs                   | `move: true`, `reschedule`                          | draft   |
| `update`                           | changes a job that has not started, and says whether it did                       | jobs                   | `Update`, which threw a conflict                    | draft   |
| `schedule`                         | a repeat the code owns, given with its handler                                    | jobs                   | `OpenSchedule`                                      | draft   |
| `run`                              | a handler's call on a job: its attempt, its steps, its answers                    | jobs                   | `job`, which also named the record                  | draft   |
| `runDue`                           | runs what is due, then returns                                                    | jobs                   | `untilIdle: true`, `drain`, which deletes in BullMQ | draft   |
| `tx`, `with`                       | a transaction that reads, then writes; `tx.with(handle)` takes a handle in        | every engine           | `batch()` in Bun and Python, `withTx`               | draft   |
| `batch`                            | statements known before they run, written as one in a shared commit               | sql                    | `db.Batch(func)` in Go, `exec([…])` in a draft      | draft   |
| `list`, `all`                      | an array in memory; an iterator over pages                                        | every engine           | `All` meant both                                    | draft   |
| `db.bucket`, `db.queue`            | kv and jobs opened from an SQL database live in its file and commit with its rows | kv, jobs               | `In`, `in`, `in_`, `database: db`                   | decided |
| `openBeside`                       | opens a directory another process holds, SQL only                                 | root                   | `Guest: true`                                       | draft   |
| `background`                       | `false` stops all periodic work                                                   | root                   | `Manual: true`                                      | draft   |

## jobs, today and the draft

```ts
// today, from docs/jobs
const refreshes = store.jobs.queue<Refresh>('refreshes', { maxRunning: 8, maxRunningInGroup: 2 })
await refreshes.enqueue({ dataset: 3 }, { key: 'refresh:3', group: 'db:42' })
await checks.enqueue(check, { key: `check:${check.id}`, after: '25h', move: true })
await probes.enqueue({ url }, { key: `probe:${id}`, repeat: { every: '30s', spread: true } })
const pushes = store.jobs.queue<Push>('pushes', { keepDone: '1h' })
await reminders.work(remind, { untilIdle: true })
```

```ts
// draft
const refreshes = store.queue<Refresh>('refreshes', { concurrency: { total: 8, group: 2 } })
await refreshes.add({ dataset: 3 }, { id: 'refresh:3', group: 'db:42' })
await checks.set(`check:${check.id}`, check, { delay: '25h' })
await probes.set(`probe:${id}`, { url }, { every: '30s' })       // ids spread over the interval
const pushes = store.queue<Push>('pushes', { dedupe: '1h' })
await reminders.runDue(remind)
reminders.work(async ({ userId, text }, run) => push(userId, text))
```

- `every` spreads ids over the interval by itself, which is almost always
  wanted; `cron` is the wall clock. `spread` disappears.
- A newcomer learns seven concepts instead of seventeen, all known from
  BullMQ: a queue, a job and its id, when it runs, the handler, retries,
  limits, how long to keep it.

## Each language's spelling

| Language   | Options                                                | Time                             | Nested settings                                |
|------------|--------------------------------------------------------|----------------------------------|------------------------------------------------|
| TypeScript | an object, camelCase                                   | `'30s'`, `Date`                  | `{ concurrency: { total: 8, group: 2 } }`      |
| Python     | keyword arguments, snake_case                          | `'30s'`, `timedelta`, `datetime` | `concurrency=Concurrency(total=8, group=2)`    |
| Go         | functional options on calls, structs for nested values | `time.Duration`, `time.Time`     | `jobs.Concurrency{Total: 8, Group: 2}`         |
| Rust       | builders                                               | `Duration`, a timestamp type     | `.concurrency(Concurrency::total(8).group(2))` |

## The API books

One book an engine, `plan/api/<engine>.md`, written before the engine's code:
the concepts a newcomer needs, every call site in Rust, TypeScript, Python and
Go, every option with its default, every error and what to do about it. jobs
first, kv second, then each engine before its phase. The protocol's schema is
written from the accepted book, and the SDKs' types are generated from the
schema.
