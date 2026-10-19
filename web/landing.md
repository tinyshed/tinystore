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

SQL, key-value state, durable jobs, files, search, logs and metrics in one
directory, with one lifecycle, one memory budget and one backup.

```ts title="app.ts" install="bun add @tinyshed/tinystore"
import { open } from '@tinyshed/tinystore'

await using store = await open('./data')

const db = await store.database('app', { migrations: './migrations' })
await db.exec`insert into users (id, name) values (${42}, ${'Ada'})`

const sessions = store.bucket<Session>('sessions', { idle: '30d' })
await sessions.set(digest(token), { userId: 42 })

const emails = store.queue<Email>('emails')
await emails.add({ to: 'ada@example.com' })

const avatars = store.files('avatars')
await avatars.put('42.png', Bun.file('avatar.png'))

store.logger('api').info('user created', { userId: 42 })
store.metrics.counter('signups_total').inc()
```

```python title="app.py" install="pip install tinyshed-tinystore"
async with await tinystore.open("data") as store:
    db = await store.database("app", migrations="migrations")
    await db.exec("insert into users (id, name) values (?, ?)", 42, "Ada")

    sessions = store.bucket("sessions", Session, idle="30d")
    await sessions.set(digest(token), Session(user_id=42))

    emails = store.queue("emails", Email)
    await emails.add(Email(to="ada@example.com"))

    avatars = store.files("avatars")
    await avatars.put("42.png", Path("avatar.png").read_bytes())

    store.logger("api").info("user created", user_id=42)
    store.metrics.counter("signups_total").inc()
```

```go title="main.go" install="go get tinyshed.org/tinystore"
// errors left out
store, err := tinystore.Open(ctx, "data")
defer store.Close()

db, err := sqldb.Open(ctx, store, "app", sqldb.Migrations(os.DirFS("migrations")))
_, err = db.Exec(ctx, "insert into users (id, name) values (?, ?)", 42, "Ada")

sessions, err := kv.OpenBucket[Session](store, "sessions", kv.Idle(30*24*time.Hour))
err = sessions.Set(ctx, digest(token), Session{UserID: 42})

emails, err := jobs.OpenQueue[Email](store, "emails")
_, err = emails.Add(ctx, Email{To: "ada@example.com"})

avatars, err := blobs.OpenFiles(store, "avatars")
_, err = avatars.Put(ctx, "42.png", avatar)

slog.New(records.NewHandler(store, "api")).Info("user created", "userId", 42)
signups, err := metrics.OpenCounter(store, "signups_total")
signups.Inc()
```

```rust title="main.rs" install="cargo add tinystore"
let store = Store::open("data", Options::default())?;

let db = store.database("app").migrations("migrations").open()?;
db.exec(tinystore::sql!("insert into users (id, name) values (?, ?)", 42, "Ada"))?;

let sessions = store.bucket::<Session>("sessions").idle(Duration::from_hours(24 * 30)).open()?;
sessions.set(digest(&token), &Session { user_id: 42 })?;

let emails = store.queue::<Email>("emails").open()?;
emails.add(&Email { to: "ada@example.com".into() })?;

let avatars = store.files("avatars").open()?;
avatars.put("42.png", &avatar)?;

tracing::info!(user_id = 42, "user created"); // through store.logger("api").layer()
store.metrics()?.counter("signups_total").open()?.inc();
```

## Engines

|                                                                      |                   |                                                                         |
|----------------------------------------------------------------------|-------------------|-------------------------------------------------------------------------|
| [SQL](../docs/chat/messages.md)                                      | Relational state  | Your own SQLite databases: checked migrations, a query builder.         |
| [KV](../docs/chat/sessions-and-limits.md)                            | Application state | Values with expiry and versions; counters, rate limits, quotas, `once`. |
| [Jobs](../docs/chat/replies.md)                                      | Background work   | Work that runs at its time: retries, steps, schedules, limits.          |
| [Blobs](../docs/chat/attachments.md)                                 | Files             | Files by path, written whole or not at all, checked when read.          |
| [Search](../docs/chat/finding-old-messages.md)                       | Words and vectors | Indexes of documents or of your tables, by words, by meaning, or both.  |
| [Channels](../docs/chat/live-updates.md)                             | Messages          | Messages to whoever listens now, sent from a transaction at its commit. |
| [Locks](../docs/chat/live-updates.md#one-summary-per-chat-at-a-time) | Coordination      | Locks and semaphores with fences, across processes and connections.     |
| [Config](../docs/chat/settings.md)                                   | Settings          | Files and the environment in layers; secrets; settings changed live.    |
| [Records](../docs/chat/what-it-is-doing.md)                          | Logs and events   | Read back by time, level, fields and text; followed as they arrive.     |
| [Metrics](../docs/start/why.md#the-engines)                          | Time series       | Samples kept bit for bit, answered exactly.                             |
| [Backup](../docs/start/why.md#the-engines)                           | The whole store   | One checked zip of every engine, taken while it runs.                   |

## Measured against what it replaces.

|          |                                                                                     |
|----------|-------------------------------------------------------------------------------------|
| Services | The stack TinyStore replaces: Redis, PostgreSQL, VictoriaMetrics and files on disk. |
| Batch    | The queue lives in the application's SQL file; a job commits with its rows.         |
| Split    | The queue has a file of its own, as by default; each file commits apart.            |

Local Linux-container medians on a Ryzen 7 7700 with Go 1.27.1. These are
workload comparisons, not universal wins: specialized KV engines read faster,
and Prometheus and VictoriaMetrics ingest more samples a second.
[Methodology and reports ↗](https://github.com/tinyshed/research/tree/main/tinystore)
