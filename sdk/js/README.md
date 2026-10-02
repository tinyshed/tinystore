# TinyStore for Bun

kv, jobs, blobs, SQL, records and metrics in one directory, served by a
sidecar the SDK starts. A Go program embeds the same engines;
[docs/sdk.md](../../docs/sdk.md) has the three languages side by side.

```sh
bun add tinystore
```

The package for this platform, an optional dependency, carries the
`tinystore` binary. `TINYSTORE_BIN`, `open`'s `binary` or `PATH` name another.
Bun 1.4 or later.

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
const log = store.records.logger('api')                  // never waits for the server
log.with({ requestId }).warn('slow request', { ms: 1200 })
log.event('user.created', { userId: 42 })

const page = await store.records.scan({ since: '1h', minLevel: 'warn', limit: 100 })
const more = await store.records.scan({ since: '1h', minLevel: 'warn', limit: 100, after: page.next })
for await (const record of store.records.all({ since: '24h', traceId })) { … }

const worker = store.records.lines('worker')             // another program's output, cut anywhere
for await (const chunk of child.stdout) worker.write(chunk)
```

A logger holds 1024 lines and hands them over every second or once half of
them wait; what does not fit is dropped and counted in `log.dropped`.

## metrics

```ts
store.metrics.counter('http_requests_total').with({ route: '/users' }).inc()
store.metrics.gauge('queue_depth').set(12)

await store.metrics.ingest({ name: 'cpu', kind: 'gauge', labels: { host: 'web-1' }, samples: [[new Date(), 0.42]] })
const series = await store.metrics.read({ name: 'cpu', match: { host: 'web-1' }, since: '1h' })
const buckets = await store.metrics.aggregate({ name: 'http_requests_total', since: '24h', width: '1h', op: 'increase' })
```

A sample comes back bit for bit, `-0` and a NaN's payload included; a range is
`since`, or `from` and `to` in unix milliseconds.

## Errors and cancellation

Every error is its code's class, a `TinystoreError`: `InvalidError`,
`ConflictError`, `LimitError`, `TooOldError` and the rest, each carrying what it
names. `work`, `put` and `get` of blobs take an `AbortSignal`; an abort is one
`CANCEL` on the wire.
