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
  takes no type parameter: `kv.OpenBucket[T](store, …)`, and
  `kv.OpenBucket[T](db, …)` for a database's.

## The newcomer check

A helper agent that has read nothing about TinyStore gets one call site from
a book, a line or a few, and says in one sentence what it does and what it
returns. The book keeps the answer beside the call. A wrong or hesitant answer
means a rename, never a comment. A book is accepted when every call site in it
passes and the owner agrees.

## Words

Decided words are settled; draft words wait for their engine's book.

| Word                               | Means                                                                                          | Where                  | Was                                                 | State   |
|------------------------------------|------------------------------------------------------------------------------------------------|------------------------|-----------------------------------------------------|---------|
| `channels`, `publish`, `subscribe` | messages between parts of an application                                                       | runtime                | signals, an idea                                    | decided |
| `durability`                       | a file's mode, `'full'` or `'os'`: a store's at `open`, a database's own                       | every engine           | FULL only, until 10 October                         | decided |
| `flushEvery`                       | kept in memory and written every span                                                          | kv counters            | `loseAtMost`, `durability: '1s'`                    | draft   |
| `concurrency`                      | how many run at once: `8` or `{ total, group }`                                                | jobs                   | `maxRunning`, `maxRunningInGroup`, `workers`        | decided |
| `rate`                             | starts or calls a span, as text `'30/s'`                                                       | jobs, kv limiter       | `Rate(n, per)` in Go                                | decided |
| `peek`, `reset`                    | a limit's answer without using it; forgetting a key                                            | kv limits              | `Get`, `Delete`, `check`                            | draft   |
| `keep`                             | how long failed or old things stay                                                             | jobs, records, metrics | `keepFailed`, `Retention`                           | draft   |
| `dedupe`                           | how long a done job's id makes adding it again add nothing                                     | jobs                   | `keepDone`                                          | draft   |
| `ttl`                              | when a key expires                                                                             | kv                     | the same                                            | draft   |
| `idle`                             | expires after a span without reads or writes                                                   | kv                     | `sliding`                                           | draft   |
| `under`                            | a branch: whose keys these are, cleared in one call                                            | kv                     | `Of`, `of`                                          | draft   |
| `id`                               | a job's identity; adding an id that is taken adds nothing                                      | jobs                   | `key`                                               | draft   |
| `group`                            | jobs that share a group's `concurrency`                                                        | jobs                   | the same                                            | draft   |
| `add`                              | puts a job in a queue                                                                          | jobs                   | `enqueue`                                           | draft   |
| `delay`, `at`                      | when a job runs: after a span, at a time                                                       | jobs                   | `after`, `at`                                       | draft   |
| `every`, `cron`                    | repeats: an interval spread by id, or the wall clock of a time zone                            | jobs                   | `every` with `spread: true`                         | draft   |
| `set`                              | makes an id's job this value at this time, whatever it was                                     | jobs                   | `move: true`, `reschedule`                          | draft   |
| `update`                           | changes a job that has not started, and says whether it did                                    | jobs                   | `Update`, which threw a conflict                    | draft   |
| `schedule`                         | a repeat the code owns, given with its handler                                                 | jobs                   | `OpenSchedule`                                      | draft   |
| `run`                              | a handler's call on a job: its attempt, its steps, its answers                                 | jobs                   | `job`, which also named the record                  | draft   |
| `runDue`                           | runs what is due, then returns                                                                 | jobs                   | `untilIdle: true`, `drain`, which deletes in BullMQ | draft   |
| `tx`, `with`                       | a transaction that reads, then writes; `tx.with(handle)` takes a handle in                     | every engine           | `batch()` in Bun and Python, `withTx`               | draft   |
| `batch`                            | statements known before they run, written as one in a shared commit                            | sql                    | `db.Batch(func)` in Go, `exec([…])` in a draft      | draft   |
| `list`, `all`                      | an array in memory; an iterator over pages                                                     | every engine           | `All` meant both                                    | draft   |
| `db.bucket`, `db.queue`            | kv and jobs opened from an SQL database live in its file and commit with its rows              | kv, jobs               | `In`, `in`, `in_`, `database: db`                   | decided |
| `openBeside`                       | opens a directory another process holds, SQL only                                              | root                   | `Guest: true`                                       | draft   |
| `background`                       | `false` stops all periodic work                                                                | root                   | `Manual: true`                                      | draft   |
| `files`, `folder`                  | a named set of files by path; the files under a path, its segments one with theirs             | blobs                  | `blobs.bucket`, `Of`                                | draft   |
| `upload`, `commit`                 | a file written a piece at a time, published whole at `commit`                                  | blobs                  | `Create`                                            | draft   |
| `head`                             | what a file carries, none of its bytes                                                         | blobs                  | `Stat`                                              | draft   |
| `create`                           | writes only where nothing is, and says whether it did                                          | kv, blobs              | `SetIfAbsent`, `IfNoneMatch("*")`                   | draft   |
| `maxFileSize`                      | the largest file a set of files takes; nothing stored is removed for room                      | blobs                  | `MaxSize`                                           | draft   |
| `logger`, `events`                 | what prints and keeps a program's lines; a named stream of events, every stream without a name | records                | `store.records`, `Handler`, `Append`                | draft   |
| `fields`, `context`                | what an event's call gave; what its logger's `with` gave                                       | records                | `Attrs`, `Context`                                  | draft   |
| `tail`, `poll`                     | events as they are written, from now; every event once, from a cursor the program keeps        | records                | `Tail`, `Follow`                                    | draft   |
| `ingest`, `latest`                 | samples sent from elsewhere; each series' newest sample in a range                             | metrics                | `Ingest`, `Latest`                                  | draft   |
| `by`, `op`                         | the labels or keys that make an aggregate's groups; what each of its buckets answers           | records, metrics       | `By`, `Op`, `Width` for `every`                     | draft   |

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

### Python and Go, as every book writes them

Written on 18 October, before the two SDKs are rewritten on protocol 2, so
that each book gives every call site in the four languages from the same
rules. The calls are TypeScript's and Rust's, word for word where the
language lets them be; what follows is only what the language changes.

| What                       | Python                                                                        | Go                                                                                  |
|----------------------------|-------------------------------------------------------------------------------|-------------------------------------------------------------------------------------|
| the package                | one, `tinystore`, on asyncio                                                  | the root `tinystore` and a package an engine: `kv`, `jobs`, `sqldb`, `blobs` …      |
| the store                  | `store = await tinystore.open("data", …)`, `async with` too                   | `store, err := tinystore.Open(ctx, "data", …)`; `defer store.Close()`               |
| a handle                   | `store.bucket("sessions", Session, idle="30d")`: no `await`, as in TypeScript | `kv.OpenBucket[Session](store, "sessions", kv.Idle(d))`: opens at its first call    |
| a handle's type            | the second argument: a dataclass, a TypedDict, a pydantic model or a builtin  | the type parameter; the handle is `*kv.Bucket[Session]`                             |
| a call that reaches it     | `await`ed                                                                     | `ctx` first, an `error` last                                                        |
| options                    | keyword arguments, TypeScript's names in snake_case                           | values of the package: `kv.TTL(time.Hour)`; a struct for a nested one               |
| nothing there              | `None`                                                                        | `found` false: `v, found, err := b.Get(ctx, key)`                                   |
| an answer                  | a dataclass, its fields in snake_case: `answer.retry_at`                      | a struct: `answer.RetryAt`                                                          |
| a page; every page         | `await b.list(limit=100, after=page.next)`; `async for e in b.all()`          | `b.List(ctx, kv.Limit(100), kv.After(page.Next))`; `for e, err := range b.All(ctx)` |
| a function the store calls | `async def`; a lambda that returns an awaitable                               | `func(ctx context.Context, …) error`; its `ctx` ends when it must stop              |
| a transaction              | `await store.tx(place)` of an `async def place(tx)`                           | `store.Tx(ctx, func(tx *tinystore.Tx) error {…})`                                   |
| a handle in it             | `tx.with_(stock)`, `with` being a keyword                                     | `stock.In(tx)`, the handle given back in the transaction                            |
| a scope                    | `async with`: an upload, a lock's hold, a timer's measure                     | a function, or `defer`                                                              |
| an error                   | `except tinystore.ConflictError as err`                                       | `errors.Is(err, tinystore.ErrConflict)`; `errors.As` into a `*tinystore.Error`      |
| a keyword taken            | a trailing `_`: `with_`, `sql.or_`, `search.or_`                              | none                                                                                |
| a query                    | keyword arguments: `read(name="cpu", since="1h", where={…})`                  | a struct, as data is: `metrics.Query{Name: "cpu", Since: time.Hour}`                |
| a logger                   | `store.logger("api")`, or `store.log_handler("api")` for `logging`            | `slog.New(records.NewHandler(store, "api"))`                                        |

- **A handle's opener in Go is `Open` and its type**, `kv.OpenBucket`,
  `jobs.OpenQueue`, `blobs.OpenFiles`, `sqldb.Open`, because a Go function
  and a type of one package cannot share a name, and the type is the one a
  program writes in its own structs: `sessions *kv.Bucket[Session]`, as
  Rust's `Bucket<Session>` and TypeScript's. Rust's builders end in
  `.open()` alike. It opens at its first call, as `sql.Open` connects at
  its first query, so it takes no `ctx`, and its `err` is a name or an
  option it refuses. What opens only once the store answers, a database
  whose migrations run and a config whose layers are read, is awaited in
  Python and takes `ctx` in Go, as TypeScript awaits it.
- **A handle of a database opens with the same opener**, given the database
  for the store: `kv.OpenBucket[Session](db, "sessions")` is TypeScript's
  `db.bucket('sessions')`.
- **Go's lists of options are values that a call takes or does not**: a
  call given an option it has no use for does not compile. A nested value
  is a struct that is an option too, `jobs.Concurrency{Total: 8, Group: 2}`.
- **A span and a time are typed**: Python takes `"30s"` or a `timedelta`, and
  an aware `datetime`; Go a `time.Duration` and a `time.Time`. A size is
  `"2GiB"` or bytes in Python, bytes in Go.
- **A function given once runs once; a function the store may run again is
  still a function.** Python's `async with` is for a scope that runs once,
  so a lock holds through it; `store.tx` takes a function, since kv's runs
  it again across the pipe, and `db.tx` the same shape for one rule.
- **Go blocks where a Go loop would**: `Work` runs until its `ctx` ends, as
  the jobs book has it; a call that answers once something is in place,
  `Subscribe`, returns then, as TypeScript's and Rust's do.
- **What prints is the language's**: Python's logger has `logging`'s
  levels and `warning`, and a handler for `logging`; Go's is `slog`'s, its
  handler the store's, and the console's alone `console.NewHandler`, which
  links no SQLite.

### Python and Go check, 18 October

Four Haiku agents with no other context: one chose blind among eleven pairs
of spellings in doubt, one said what each of 65 bare call sites does and
returns, and two reviewed the books' Python and their Go apart, as a
developer of each language choosing a library. A first free review of both
at once stopped at the model's output limit.

| They found                                                                                   | Change                                                                 |
|----------------------------------------------------------------------------------------------|------------------------------------------------------------------------|
| `log.with_(…)` read as a `with` block, by two of four                                        | `log.bind(…)`, structlog's and loguru's word                           |
| `defer t.Since(time.Now())` read as "since now", zero                                        | `defer t.Start().Stop()`, as Prometheus' `NewTimer(h).ObserveDuration` |
| `found` named a search's answer, where Go's `found` is the bool of every `Get`               | `result`, and `search.Result[T]`                                       |
| `metrics.Latest` a function and a type of one package                                        | the type is `metrics.Point`                                            |
| two `Work` calls in a row, the first of which blocks                                         | the example runs them in an `errgroup`                                 |
| examples that did not compile: a page from itself, `:=` with no new name, an id concatenated | fixed, and every Go block checked for a name declared twice            |
| an error of `TryHold` unchecked, an index opened and dropped                                 | the examples check and keep them                                       |
| `out.write(chunk)` not awaited where `upload.write` is                                       | `await out.write(chunk)`                                               |
| `db.scalar` with no type where `all` and `one` take one first                                | `db.scalar(int, …)`                                                    |
| `entry.version` read off an entry that may be absent                                         | the examples check it first                                            |

Read right and kept, by the blind choice: `kv.OpenBucket[T]` and its
`*kv.Bucket[T]` (over `kv.Bucket[T]` with another type, and `kv.NewBucket`,
which reads as I/O-free and as making an empty bucket), Go's `Work` blocking
until `ctx` ends, Python's transaction as a function, a lock held through
`async with`, `labels(…)`, `metrics.OpenCounter` returning its error, a query
as a struct, `v, found, err`, and Python's type as the second argument. The
bare call sites were read at four of five or better but for ten, the
lowest a `try_hold` through `async with`, which the book now says.

Kept as the TypeScript and Rust books have them, which the reviews asked to
change: `lastInsertRowid` over Go's `LastInsertId`, `Keep` beside `TTL`,
sqldb's `all` as a list where kv's is an iterator, one name a page's items
(`entries`, `rows`, `jobs`), spans as text in Python, and a store's backup
leaving the key out unless asked.

Decided by the owner the same day: **Go's handle in a transaction is
`stock.In(tx)`**, a method a Go reader finds after `stock.`, as
`time.Time.In` gives a time in a zone, which the blind choice took over
`tinystore.With(tx, stock)` and the Go review asked for. TypeScript, Python
and Rust keep `tx.with(stock)`, which kv's first check chose over an `in(tx)`
it read as a filter.

For the owner:

- **Config in Go's root package**, `tinystore.OpenConfig`, which the Go
  review would move to `config`: a package `config` takes the name a
  program gives its value, `config, err := config.Open(…)`.

## The API books

One book an engine, `plan/api/<engine>.md`, written before the engine's code:
the concepts a newcomer needs, every call site in Rust, TypeScript, Python and
Go, every option with its default, every error and what to do about it. jobs
first, kv second, then each engine before its phase. The protocol's schema is
written from the accepted book, and the SDKs' types are generated from the
schema.
