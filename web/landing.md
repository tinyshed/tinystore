<!--
The landing page's words, read when the site is built: the heading is the
headline, the paragraph after it the pitch, the fences the sample, one a
language with its install command in install="…", the table under Engines
the engine list, and the section after it the numbers' heading, legend and
caveat. The design is web/src/routes/+page.svelte, and the numbers' cards are
read from the README's SVGs. Every call in the sample is the SDK's own: check
it against docs/reference/ before changing it. An engine links its overview
page once one is written, and its package README until then. The README shows
the headline, the pitch, the sample and the engines too, and so do the SDK
packages' READMEs in their own language: task readme writes them there from
this file.
-->

# A small storage runtime for applications.

SQL, key-value state, durable jobs, files, metrics and logs in one directory,
with one lifecycle, one memory budget and one backup.

```ts title="app.ts" install="bun add @tinyshed/tinystore"
import { open } from '@tinyshed/tinystore'

await using store = await open('./data')

const db = await store.sql('app', {
	migrations: { '0001_users.sql': 'create table users (id integer primary key, name text)' },
})
await db.exec`insert into users (id, name) values (${42}, ${'Ada'})`

const sessions = store.kv.bucket<string>('sessions', { sliding: '30d' })
await sessions.of('42').set('token', 'abc123')

const emails = store.jobs.queue<{ to: string }>('emails')
await emails.enqueue({ to: 'ada@example.com' })

const files = store.blobs.bucket('files')
await files.put('avatars/42.png', Bun.file('avatar.png'))

const log = store.records.logger('api')
log.event('user.created', { userId: 42 })

const signups = store.metrics.counter('signups_total')
signups.inc()
```

```python title="app.py" install="pip install tinyshed-tinystore"
import asyncio
import logging
from pathlib import Path

import tinystore


async def main() -> None:
    async with tinystore.open("./data") as store:
        db = await store.sql(
            "app", migrations={"0001_users.sql": "create table users (id integer primary key, name text)"}
        )
        await db.exec("insert into users (id, name) values (?, ?)", 42, "Ada")

        sessions = store.kv.bucket("sessions", str, sliding="30d")
        await sessions.of("42").set("token", "abc123")

        emails = store.jobs.queue("emails", dict[str, str])
        await emails.enqueue({"to": "ada@example.com"})

        files = store.blobs.bucket("files")
        await files.put("avatars/42.png", Path("avatar.png").read_bytes())

        logging.getLogger().addHandler(store.records.handler("api"))
        logging.info("user created", extra={"user_id": 42})

        store.metrics.counter("signups_total").inc()


asyncio.run(main())
```

```go title="main.go" install="go get github.com/tinyshed/tinystore"
// errors left out
store, err := tinystore.Open(ctx, "./data", tinystore.Options{})
defer store.Close(ctx)

db, err := sqldb.Open(ctx, store, "app", migrations, schema)
_, err = db.Exec(ctx, `insert into users (id, name) values (?, ?)`, 42, "Ada")

state, err := kv.Open(ctx, store, kv.Options{})
sessions, err := kv.OpenBucket[string](ctx, state, "sessions", kv.Sliding(30*24*time.Hour))
err = sessions.Of("42").Set(ctx, "token", "abc123")

queues, err := jobs.Open(ctx, store, jobs.Options{})
emails, err := jobs.OpenQueue[Email](ctx, queues, "emails")
err = emails.Enqueue(ctx, Email{To: "ada@example.com"})

objects, err := blobs.Open(ctx, store, blobs.Options{})
files, err := blobs.OpenBucket(ctx, objects, "files")
_, err = files.Put(ctx, "avatars/42.png", avatar)

logs, err := records.Open(ctx, store, records.Options{})
log := slog.New(logs.Handler("api"))
log.Info("user created", "userId", 42)

series, err := metrics.Open(ctx, store, metrics.Options{})
series.Counter("signups_total").Inc()
```

## Engines

| | | |
|---|---|---|
| [SQL](../docs/sql/README.md) | Relational state | The application's own SQL databases: tables from structs, checked migrations. |
| [KV](../docs/kv/README.md) | Application state | Current state: typed buckets, counters, expiry, versions. |
| [Jobs](../docs/jobs/README.md) | Durable background work | Work that runs at its time: retries, leases, repeats. |
| [Blobs](../docs/blobs/README.md) | Files and objects | Files by path, checked when read whole. |
| [Records](../docs/records/README.md) | Logs and events | Read by time, level and keys. |
| [Metrics](../docs/metrics/README.md) | Time series | Samples kept bit for bit, answered exactly. |

## Measured against what it replaces.

| | |
|---|---|
| Services | The stack TinyStore replaces: Redis, PostgreSQL, VictoriaMetrics and files on disk. |
| Batch | The queue lives in the application's SQL file; a job commits with its rows. |
| Split | The queue has a file of its own, as by default; each file commits apart. |

Local Linux-container medians on a Ryzen 7 7700 with Go 1.27.1. These are
workload comparisons, not universal wins: specialized KV engines read faster,
and Prometheus and VictoriaMetrics ingest more samples a second.
[Methodology and reports ↗](https://github.com/tinyshed/research/tree/main/tinystore)
