# Bun and Node API

Every call of the Bun and Node SDK, `@tinyshed/tinystore`, by engine. Each
section shows the calls in one block and then explains what they guarantee.
The guides explain each feature in more detail, with examples in Bun, Python
and Go side by side.

This page shows Bun and Node only. The [Python API](python.md) is the same
list for Python.

## Install

```sh
bun add @tinyshed/tinystore
```

The package runs on Bun 1.4 or later and on Node 22 or later, with one API.
Bun runs the package's TypeScript, and Node runs the JavaScript that the
package contains.

An optional dependency for your platform contains the `tinystore` binary. To
use another binary, set `TINYSTORE_BIN`, pass `binary` to `open`, or put
`tinystore` on your `PATH`.

The binary is also the `tinystore` command:

```sh
bunx @tinyshed/tinystore status ./data
bunx @tinyshed/tinystore logs ./data -f
claude mcp add tinystore -- bunx @tinyshed/tinystore mcp ./data
```

To use the SDK from a checkout of the repository, run
`bun add ../tinystore/sdk/js`. A release builds the type declarations, so a
checkout has none. Set `"customConditions": ["bun"]` and
`"allowImportingTsExtensions": true` in `tsconfig.json`, and TypeScript checks
the SDK's source instead.

## Open a store

```ts
import { connect, open } from '@tinyshed/tinystore'

await using store = await open('./data')                 // the directory's sidecar, started if none runs
await using alone = await open(dir, { private: true })   // a child of this process, for tests and scripts
await using remote = await connect('tls://db.internal:7070', { token })

await using test = await open(dir, { private: true, clock: new Date('2026-10-03T09:00:00Z') })
await test.clock.advance('1h')                           // keys expire and jobs come due without waiting

await store.backup('backup.zip')                         // every engine in one checked zip
```

`open` returns after the server has answered. If the server can't serve the
directory, `open` fails.

`open` takes a directory, never an address. If a server already serves the
directory, for example `tinystore serve ./data`, `open('./data')` finds it
through the directory's `SERVE` file. An address such as `tcp://…` or
`pipe:…` fails with `InvalidError`, which says what to call instead.

When you close the store, the SDK first sends the lines that loggers still
hold and the last values of the instruments. A sidecar exits after its last
connection has been idle for 30 seconds.

`tinystore restore` restores a backup into an empty directory. See
[Backups](../running/backups.md).

## KV

```ts
const sessions = store.kv.bucket<Session>('sessions', { sliding: '30d' })
await sessions.of(user.id).set(token, session)
const found = await sessions.of(user.id).get(token)      // undefined if absent or expired
const { created, entry } = await sessions.of(user.id).setEntryIfAbsent(token, session) // this one, or the one already there

const views = store.kv.counters('views')
await views.add('/home')

const page = await sessions.scan({ limit: 100 })         // { items, next }: pass next as after for the next page
for await (const entry of sessions.all()) { … }

const userId = await store.kv.tx(async tx => {           // runs again if a key it read has changed
	const userId = await codes.withTx(tx).take(digest(code))
	if (userId === undefined) throw new InvalidCodeError()
	sessions.withTx(tx).of(userId).set(digest(token), session)
	return userId
})
```

You give a bucket's type once, when you open it. It can be:

- a TypeScript type, stored as JSON;
- a kind: `'string'`, `'bytes'`, `'bigint'` and others;
- a Standard Schema, such as a zod schema, which checks every value that you
  read.

`setIfAbsent` and `setEntryIfAbsent` write only if no live key exists, in one
write. If two processes create the same key at once, for example a daily salt
or a claim, only one of them succeeds. A `get` followed by a `set` can't
guarantee that, because both processes may write.

See [KV](../kv/README.md), [Transactions](../kv/transactions.md) and
[Sessions](../kv/sessions.md).

### Configs, rate limits and quotas

```ts
import file from './config.yaml'

const cfg = await store.kv.config('app', { port: 8080, dbUrl: '', origins: ['localhost'] }, { file })
cfg.value.port                       // 3000 if .env sets PORT=3000
await cfg.update({ port: 4000 })     // stored: 4000 after a restart too, in every process at once
await cfg.reset('port')              // back to 3000 from .env
cfg.watch(c => server.setPort(c.port))

const limit = store.kv.limiter('api', { rate: '100/s', burst: 20 })
const { ok, retryAfter } = await limit.of(tenant).allow(userId)

const ai = store.kv.quota('ai', { session: '100/5h', weekly: '300/7d' })
const usage = await ai.allow(user.id)    // counts in every window or in none: usage.windows.weekly.left
```

A config takes its fields and their types from its defaults. Each layer
overrides the one before it:

1. the defaults;
2. the values from `file`;
3. the environment;
4. the values that `update` stored.

Each field reads the variable with its name in upper snake case: `dbUrl` reads
`DB_URL`, after `prefix` if you set one. To use another name, pass
`env: { dbUrl: 'DATABASE_URL' }`. A variable is read as the default's type: a
number, `true`, a list such as `a.com,b.com`, or JSON. If it doesn't parse,
`config` fails with `InvalidError`, which names the variable. Fields in
`secret` are never stored, and `schema` checks each change.

A quota counts a use in all its windows, or in none of them. Each window
starts at a key's first use. `get` reads the windows without using anything,
and `refund` gives uses back.

See [Configs](../kv/configs.md), [Rate limits](../kv/rate-limits.md) and
[Quotas](../kv/quotas.md).

### Once

```ts
const charges = store.kv.once<Receipt>('charges')               // answers are stored for a day
const receipt = await charges.run(requestId, () => pay.charge(order, requestId))
```

`run` returns the answer stored under the key. If there is none, it runs the
function and stores its answer. If another call with the same key arrives
meanwhile, from any client, it waits and gets the same answer. If the
function throws, nothing is stored, and the next call runs it again.

The function runs once per key only while its call is alive. If the function
has an effect outside the store, such as a charge, pass the key to that
service too. See [Once](../kv/once.md).

## Jobs

```ts
const reminders = store.jobs.queue<{ note: number }>('reminders')
await reminders.enqueue({ note: 1 }, { after: '1h', key: 'note/1' })

await reminders.work(async job => {
	await remind(job.value.note, { signal: job.signal })
}, { workers: 4, signal })
```

If the handler returns, the job is done. If it throws, the job is retried
later, and the wait grows with each attempt. To decide yourself, call
`job.retry`, `job.fail` or `job.snooze`. `work` runs until its signal aborts,
and lets the jobs in progress finish first.

```ts
const videos = store.jobs.queue<Video>('videos', { maxRunning: 2 })
await videos.enqueue(video, { key: video.id })
for await (const s of videos.watch(video.id)) send(s.state, s.ahead, s.progress) // waiting 3 … running 0.4 … done

await videos.work(async job => {
	await transcode(job.value, { signal: job.signal, onProgress: p => job.progress(p) })
})
```

`get` returns the job's state:

| State | What it includes |
|---|---|
| `waiting` | `ahead`: how many jobs run before it |
| `running` | `progress`: the value the handler reported last |
| `failed` | |
| `done` | only while `keepDone` keeps the key |
| `cancelled` | |

`watch` yields the state again at each change until the job ends. `ran` and
`took` tell when the last finished run started and how many milliseconds it
took. For a schedule, `at` is the next run.

`job.progress` accepts any JSON up to 4 KiB, and sends the latest value at
most ten times per second. `cancel` also stops a running job: its
`job.signal` aborts with a `CancelledError`, and whatever the handler returns
after that changes nothing. `maxRunning` limits how many jobs of the queue run
at once, across all workers of the store.

A step stores its answer, so the next attempt of the same run doesn't run it
again:

```ts
const hits = await job.step('search', () => search(q))
```

See [Jobs](../jobs/README.md), [Watching a job](../jobs/watching.md) and
[Steps](../jobs/steps.md).

## Blobs

```ts
const files = store.blobs.bucket('files')
await files.put('avatars/42.png', Bun.file('avatar.png'))
const avatar = await files.get('avatars/42.png')          // undefined if absent
const bytes = await avatar?.bytes()                       // a whole read checks every byte
```

See [Blobs](../blobs/README.md).

## SQL

```ts
const app = await store.sql('app', { migrations: './migrations' })
await app.exec`insert into notes (body) values (${body})`
const notes = await app.all<Note>`select id, body from notes where author = ${author}`
await app.batch(tx => {                                   // one transaction: all or nothing
	tx.exec`update notes set body = ${body} where id = ${id}`
	tx.exec`insert into edits (note) values (${id})`
})
const [page, total] = await app.view(tx => [tx.all`select * from notes limit 20`, tx.scalar`select count(*) from notes`])

const index = store.jobs.queue<{ id: number }>('index', { in: app }) // the queue is stored in sql/app.db
await app.batch(tx => {
	tx.exec`update notes set body = ${body} where id = ${id}`
	index.withTx(tx).enqueue({ id })                       // commits with the update, or not at all
})
```

Without `migrations`, `store.sql` opens the file as it is, and creates an
empty one if there is none. Nothing is applied or checked, which is handy for
a first try: ``await (await store.sql('scratch')).scalar<number>`select 1` ``.

The values in a template are passed as arguments, never as SQL. A value comes
back as SQLite stores it. See [SQL](../sql/README.md) and
[Jobs and your data](../jobs/your-data.md).

## Records

```ts
const log = store.records.logger('api', { redact: ['password'] }) // never waits for the server
log.with({ requestId }).warn('slow request', { ms: 1200 })
log.event('user.created', { userId: 42 })
await withTrace({ traceId, spanId }, async () => log.info('charged')) // the record gets the trace

const page = await store.records.scan({ since: '1h', minLevel: 'warn', limit: 100 })
const resets = await store.records.scan({ since: '1h', search: 'connection reset' }) // case-insensitive
const more = await store.records.scan({ since: '1h', minLevel: 'warn', limit: 100, after: page.next })
for await (const record of store.records.all({ since: '24h', traceId })) { … }
const { ms } = fields(page.items[0].attrs)               // a record's [key, json] pairs, parsed

const worker = store.records.lines('worker')             // another program's output, split anywhere
for await (const chunk of child.stdout) worker.write(chunk)
```

A logger buffers up to 1024 lines. It sends them every second, or as soon as
half of the buffer is full. If the buffer is full, a new line is dropped,
counted in `log.dropped` and reported on stderr. Use a logger for lines you
can afford to lose in a burst. For records that you must keep, call
`records.append`, which returns after the records are stored. A larger
`buffer` holds a longer burst.

A logger also writes each line to stderr: pretty on a terminal, and one JSON
object per line otherwise. The bytes are the same as Go's and Python's
loggers write. To change this, set `console` to `'pretty'`, `'json'` or
`'off'`, or set `stdout: true`. A logger of events, such as one view per
request, usually sets `'off'`, so it doesn't fill the program's log.

`redact` hides the values of fields with those names, at any depth and in any
case, both in the store and on the console.

To log to the console without a store, create a logger without one, and pass
it or its children to the code that needs it:

```ts
import { logger } from '@tinyshed/tinystore'

const log = logger('app', { redact: ['password'] })   // the console only, nothing stored
const db = log.with({ module: 'db' })
```

See [Records](../records/README.md) and [Logging](../records/logging.md).

## Metrics

```ts
store.metrics.counter('http_requests_total').with({ route: '/users' }).inc()
store.metrics.gauge('queue_depth').set(12)
const latency = store.metrics.timer('http_request_ms')
const user = await latency.with({ route: '/users' }).measure(() => users.get(id))

await store.metrics.ingest({ name: 'cpu', kind: 'gauge', labels: { host: 'web-1' }, samples: [[new Date(), 0.42]] })
const series = await store.metrics.read({ name: 'cpu', match: { host: 'web-1' }, since: '1h' }) // columns: times, values
const failing = await store.metrics.read({ name: 'http_requests_total', since: '1h', where: { status: oneOf('500', '502') } })
const buckets = await store.metrics.aggregate({ name: 'http_requests_total', since: '24h', width: '1h', op: 'increase' })
const routes = await store.metrics.aggregate({ name: 'http_requests_total', since: '24h', width: '1h', op: 'rate', by: ['route'] })
```

`read` returns each series as two columns: `times` in Unix milliseconds and
`values` as a `Float64Array`. `ingest` accepts the same columns, or
`[time, value]` pairs. A sample comes back bit for bit, including `-0` and a
NaN's payload. A range is either `since`, or `from` and `to` in Unix
milliseconds. `metrics.explain(range)` tells how much of its limits a read or
an aggregate would use, before it runs.

A timer's `measure(fn)` returns what `fn` returns and throws what it throws,
and records the time in both cases. `record(ms)` adds a duration that you
measured yourself. At every flush, a timer writes three series:

| Series | What it contains |
|---|---|
| `http_request_ms_count` | a counter of the measured calls |
| `http_request_ms_sum` | a counter of their total time |
| `http_request_ms_max` | the longest time since the previous flush |

The mean over a range is the increase of the sum divided by the increase of
the count.

If the store refuses a series, for example because of a label it can't
store, the instrument stops writing it and reports it once on stderr. The
other instruments keep working. See [Metrics](../metrics/README.md) and
[Instruments](../metrics/instruments.md).

## Errors and cancellation

Every error is a `TinystoreError`, with a class per error code:
`InvalidError`, `ConflictError`, `LimitError`, `TooOldError` and others. Each
error carries the details of what failed. A `LimitError` tells which limit it
hit, what the call asked for and the limit's value. See
[Errors](../concepts/errors.md).

Every call inside `withSignal(signal, fn)` ends when the signal aborts, and
the SDK sends one `CANCEL` to the server. `work`, and the blobs `put` and
`get`, also accept a signal of their own.

`await store.status()` returns the server's version, its protocol and its
engines.

Some failures happen when no call is waiting, so the SDK writes them to
stderr, as a logger with the stream `tinystore`:

- a failed instrument flush or logger write, and its recovery;
- a gauge function that threw;
- a refused instrument;
- the lines that a full logger dropped.

A repeated failure is reported again after ten minutes, or when it changes.
Go's store logs its own failures the same way.

## See also

- [Go, Bun and Python](../languages.md): how each language reaches a store
- [The sidecar](../running/sidecar.md) and [A remote server](../running/server.md)
- [Limits and defaults](limits.md)
- the SDK's source, `sdk/js/src/`, and its tests, `sdk/js/test/`
