<!-- The landing page's sample, one fence a language. Every call here is the
SDK's own; check one against docs/sdk.md before changing it. -->

```ts title="app.ts" install="bun add tinystore"
import { open } from 'tinystore'

await using store = await open('./data')

const db = await store.sql('app')
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
        db = await store.sql("app")
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
