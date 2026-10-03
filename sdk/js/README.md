# TinyStore for Bun and Node

kv, jobs, blobs, SQL, records and metrics in one directory, served by a
sidecar the SDK starts. A Go program embeds the same engines;
[docs/sdk.md](../../docs/sdk.md) has the three languages side by side.

```sh
bun add tinystore
```

The package for this platform, an optional dependency, carries the
`tinystore` binary. `TINYSTORE_BIN`, `open`'s `binary` or `PATH` name another.
Bun 1.4 or later, or Node 22 or later, with one API: Bun runs the package's
TypeScript, and Node the JavaScript it carries.

The binary is the `tinystore` command too: `bunx tinystore status ./data`,
`bunx tinystore logs ./data -f`, and for an AI agent
`claude mcp add tinystore -- bunx tinystore mcp ./data`.

## Open

```ts
import { connect, open } from 'tinystore'

await using store = await open('./data')                 // the directory's sidecar, started when none runs
await using alone = await open(dir, { private: true })   // a child of this process, for tests and scripts
await using remote = await connect('tls://db.internal:7070', { token })
```

`open` returns once the server has answered, so a directory that cannot be
served fails there. Closing hands over the loggers' lines and the instruments'
last values; a sidecar leaves once its last connection has been idle 30 s.

## kv

```ts
const sessions = store.kv.bucket<Session>('sessions', { sliding: '30d' })
await sessions.of(user.id).set(token, session)
const found = await sessions.of(user.id).get(token)      // undefined when absent or expired

const views = store.kv.counters('views')
await views.add('/home')

const page = await sessions.scan({ limit: 100 })         // { items, next }; next goes back as after
for await (const entry of sessions.all()) { … }
```

A bucket's type is given once, where it opens: JSON of `T`, a kind
(`'string'`, `'bytes'`, `'bigint'`, …) or a Standard Schema such as zod's,
which checks every read.

```ts
import file from './config.yaml'

const cfg = await store.kv.config('app', { port: 8080, dbUrl: '', origins: ['localhost'] }, { file })
cfg.value.port                       // PORT=3000 in .env makes it 3000
await cfg.update({ port: 4000 })     // kept: 4000 after a restart too, at once in every process
await cfg.reset('port')              // back to .env's 3000
cfg.watch(c => server.setPort(c.port))

const limit = store.kv.limiter('api', { rate: '100/s', burst: 20 })
const { ok, retryAfter } = await limit.of(tenant).allow(userId)

const ai = store.kv.quota('ai', { session: '100/5h', weekly: '300/7d' })
const usage = await ai.allow(user.id)    // one of each window, or none: usage.windows.weekly.left
```

A config is shaped and typed by its defaults: a file's values, then the
environment, then what `update` kept go over them, a variable named by its
field, `dbUrl` as `DB_URL`, after `prefix` when given. A number, `true`, a
list `a.com,b.com` or JSON read as the default's kind, and one that does not
is `InvalidError` at open, naming it. `env: { dbUrl: 'DATABASE_URL' }` names a
variable itself; `secret` fields are never kept; `schema` checks each change.
A quota's windows count a use together or not at all, each from a key's first
use; `get` reads them without using any, `refund` gives uses back.

```ts
const charges = store.kv.once<Receipt>('charges')               // answers kept a day
const receipt = await charges.run(requestId, () => pay.charge(order, requestId))
```

`run` returns the answer kept under a key, or runs the function and keeps
its answer: a call of the key meanwhile, from any client, waits for it and
gets the same answer, and a throw keeps nothing, so the next call runs again.
The function runs once a key while its call lives; an effect outside the
store, a charge, carries the key too.

## jobs

```ts
const reminders = store.jobs.queue<{ note: number }>('reminders')
await reminders.enqueue({ note: 1 }, { after: '1h', key: 'note/1' })

await reminders.work(async job => {
	await remind(job.value.note, { signal: job.signal })
}, { workers: 4, signal })
```

A handler's return acknowledges its job and a throw retries it, waiting longer
each time; `job.retry`, `job.fail` and `job.snooze` say otherwise. `work` runs
until its signal aborts; the jobs in hand finish first.

```ts
const videos = store.jobs.queue<Video>('videos', { maxRunning: 2 })
await videos.enqueue(video, { key: video.id })
for await (const s of videos.watch(video.id)) send(s.state, s.ahead, s.progress) // waiting 3 … running 0.4 … done

await videos.work(async job => {
	await transcode(job.value, { signal: job.signal, onProgress: p => job.progress(p) })
})
```

`get` says where a job is: `waiting`, with how many jobs run `ahead` of it,
`running`, with the `progress` its handler last reported, `failed`, or `done`
while `keepDone` keeps its key; `watch` yields it again at each change until
it ends, `cancelled` included, and `ran` and `took` say when the last run a
handler finished began and how many milliseconds it took: a schedule's last
run beside its next, `at`. `job.progress` takes any JSON within 4 KiB and
sends the latest at most ten times a second. `cancel` takes a running job too:
its `job.signal` aborts with a `CancelledError`, and what the handler returns
settles nothing. `maxRunning` bounds the jobs running at once across every
worker of the store.

A step of a job's run keeps its answer, so that the attempt after a failure
does not run it again: `const hits = await job.step('search', () => search(q))`.

## blobs

```ts
const files = store.blobs.bucket('files')
await files.put('avatars/42.png', Bun.file('avatar.png'))
const avatar = await files.get('avatars/42.png')          // undefined when absent
const bytes = await avatar?.bytes()                       // a whole read checks every byte
```

## SQL

```ts
const app = await store.sql('app', { migrations: './migrations' })
await app.exec`insert into notes (body) values (${body})`
const notes = await app.all<Note>`select id, body from notes where author = ${author}`
await app.batch(tx => {                                   // one transaction, all or none
	tx.exec`update notes set body = ${body} where id = ${id}`
	tx.exec`insert into edits (note) values (${id})`
})
```

A value comes back as SQLite keeps it; a template's values are arguments,
never SQL.

## records

```ts
const log = store.records.logger('api', { redact: ['password'] }) // never waits for the server
log.with({ requestId }).warn('slow request', { ms: 1200 })
log.event('user.created', { userId: 42 })
await withTrace({ traceId, spanId }, async () => log.info('charged')) // the record carries the trace

const page = await store.records.scan({ since: '1h', minLevel: 'warn', limit: 100 })
const resets = await store.records.scan({ since: '1h', search: 'connection reset' }) // case ignored
const more = await store.records.scan({ since: '1h', minLevel: 'warn', limit: 100, after: page.next })
for await (const record of store.records.all({ since: '24h', traceId })) { … }
const { ms } = fields(page.items[0].attrs)               // a record's [key, json] pairs, as JSON.parse reads them

const worker = store.records.lines('worker')             // another program's output, cut anywhere
for await (const chunk of child.stdout) worker.write(chunk)
```

A logger holds 1024 lines and hands them over every second or once half of
them wait; what does not fit is dropped, counted in `log.dropped` and said on
stderr.

Each line also goes to stderr as it is logged: pretty on a terminal, one JSON
object a line otherwise, the bytes Go's and Python's loggers write.
`console: 'pretty' | 'json' | 'off'` and `stdout: true` choose; `redact`
hides the values of fields of those names, at any depth, the case ignored, in
the store and on the console. A program that wants the logger and not the
records takes one without a store, and passes it, or a child, to what needs
it:

```ts
import { logger } from 'tinystore'

const log = logger('app', { redact: ['password'] })   // the console alone, nothing kept
const db = log.with({ module: 'db' })
```

## metrics

```ts
store.metrics.counter('http_requests_total').with({ route: '/users' }).inc()
store.metrics.gauge('queue_depth').set(12)
const latency = store.metrics.timer('http_request_ms')
const user = await latency.with({ route: '/users' }).measure(() => users.get(id))

await store.metrics.ingest({ name: 'cpu', kind: 'gauge', labels: { host: 'web-1' }, samples: [[new Date(), 0.42]] })
const series = await store.metrics.read({ name: 'cpu', match: { host: 'web-1' }, since: '1h' })
const failing = await store.metrics.read({ name: 'http_requests_total', since: '1h', where: { status: oneOf('500', '502') } })
const buckets = await store.metrics.aggregate({ name: 'http_requests_total', since: '24h', width: '1h', op: 'increase' })
const routes = await store.metrics.aggregate({ name: 'http_requests_total', since: '24h', width: '1h', op: 'rate', by: ['route'] })
```

A sample comes back bit for bit, `-0` and a NaN's payload included; a range is
`since`, or `from` and `to` in unix milliseconds. `metrics.explain(range)` says
what a read or an aggregate would spend of its limits before it runs.

A timer's `measure(fn)` answers what `fn` answered and throws what it threw,
recording the time either way; `record(ms)` adds a duration of its own. Every
flush writes `http_request_ms_count` and `http_request_ms_sum`, counters, and
the longest since the flush before, `http_request_ms_max`, so that a range's
mean is the increase of its sum over the increase of its count. A series the
store refuses, a label it cannot keep, is left out from then on and said
once, and the other instruments go on being written.

## Errors and cancellation

Every error is its code's class, a `TinystoreError`: `InvalidError`,
`ConflictError`, `LimitError`, `TooOldError` and the rest, each carrying what it
names; a `LimitError` names the bound, what the call wanted and the bound.
Every call inside `withSignal(signal, fn)` ends when the signal aborts, one
`CANCEL` on the wire; `work`, `put` and `get` of blobs also take a signal of
their own. `await store.status()` says what the server is: its version, its
protocol, its engines.

What fails where no call waits is said on stderr, as a logger of stream
`tinystore` writes a line: an instrument's flush or a logger's write that
failed, and its recovery; a gauge's function that threw; a refused
instrument; the lines a full logger dropped. A failure repeated is said again
ten minutes on, or when it changes, as Go's store logs its own.
