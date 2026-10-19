# The chat we build

The chapters of Building the chat write one application: the backend of an
AI chat, where people sign in, send messages and get answers from a language
model. This chapter shows the whole plan: what the chat does, which engine
handles each part, and which chapter adds it.

## Why a chat

A chat is small enough to read in an afternoon, and it meets most of the
problems a real backend meets:

- **A request arrives twice.** A phone on a bad network sends a message,
  times out and sends it again. The chat must not answer it twice.
- **Work is slow and fails.** A model takes seconds to answer and sometimes
  fails. A request cannot wait for it, and a failed call must be retried.
- **A process dies at a bad moment.** A deploy restarts the server while a
  reply is half written. The reply must still arrive.
- **Users share limits.** A free plan allows 100 messages per 5 hours and 300
  per week, across every device of the user.
- **Several tabs watch one chat.** A message sent from a phone appears in the
  laptop's open tab at once.

Each of these is usually a service, a table and some careful code. In the
chat, each is a few lines on top of an engine.

## What the chat does

| Route or process              | What it does                                          | Engines            |
|-------------------------------|-------------------------------------------------------|--------------------|
| `POST /sign-in`               | sends a one-time code, then starts a session          | KV                 |
| `POST /chats/:id/messages`    | accepts a message once, checks the plan's limits      | SQL, KV, Jobs      |
| the reply worker              | asks the model, keeps its finished steps, saves it    | Jobs, SQL, Metrics |
| `GET /chats/:id/events`       | streams new messages and the reply's progress to tabs | Channels, Jobs     |
| `POST /chats/:id/attachments` | stores an uploaded file and serves it back            | Blobs              |
| `GET /search?q=`              | finds old messages by words and by meaning            | Search             |
| the summary job               | writes one summary per chat at a time                 | Jobs, Locks        |
| the model's settings          | its name, its limits and a switch to turn it off      | Config             |
| everything                    | logs each request, times each model call              | Records, Metrics   |
| at night                      | backs up the whole store                              | Backup             |

A message travels through the chat like this:

```text
browser ── POST /chats/7/messages ──► once     the same request twice gets one answer
                                      quota    100 per 5 hours and 300 per week, or "try later"
                                      sql      insert the message ┐ one commit
                                      jobs     add its reply job  ┘
reply worker ◄─ the job ─────────────  jobs     runs it, retries it, keeps finished steps
             ── the answer ─────────► sql      insert the reply
                                      channels tell every open tab
browser ◄── server-sent events ──────
```

## The chapters

| Chapter                   | The chat gains                                                             |
|---------------------------|----------------------------------------------------------------------------|
| Messages                  | a database with migrations, the messages table, writes and queries         |
| Sessions and limits       | sign-in codes, sessions that expire when idle, quotas, requests sent twice |
| Replies in the background | a queue in the chat's database, a worker, retries, steps, progress         |
| Attachments               | uploads written whole or not at all, served with ranges, expiry            |
| Finding old messages      | an index of the messages table, by words, by vectors, with typos           |
| Live updates              | a new message in every open tab; one summary job per chat at a time        |
| Settings                  | the model's settings from a file and the environment, some changed live    |
| What it is doing          | a logger, a trace per request, reading the logs back                       |
| How fast it is            | timers and counters, aggregates, the store's own metrics                   |
| Keeping it                | a backup while it runs, a restore, tests on a clock you move by hand       |

Every chapter starts from the chat as the previous one left it, adds one
capability, and ends with a program that runs. You can stop after any
chapter. The code is shown in TypeScript, Python, Go and Rust, and the four
versions behave the same way, because they run the same core.

## What it starts from

The chat opens one store and one database, as [One directory, many
engines](directory.md) showed:

```ts
import { open } from '@tinyshed/tinystore'

await using store = await open('./data')
const db = await store.database('chat', { migrations: './migrations' })
```

```python
async with await tinystore.open("data") as store:
    db = await store.database("chat", migrations="migrations")
```

```go
store, err := tinystore.Open(ctx, "data")
if err != nil {
	return err
}
defer store.Close()
db, err := sqldb.Open(ctx, store, "chat", sqldb.Migrations(os.DirFS("migrations")))
```

```rust
let store = Store::open("data", Options::default())?;
let db = store.database("chat").migrations("migrations").open()?;
```

Everything else opens from these two handles, as each chapter needs it.

## What it will not need

At the end of the book the chat runs on one machine with no Redis, no queue
service, no object storage, no log shipper and no metrics server. It also has
no `expires_at` columns, no cleanup cron jobs, no status table for replies,
no retry loops and no table of seen request ids. Each of those is a feature
of an engine.

When the chat outgrows one machine, Beyond one machine moves it onto three:
one that writes, and two in other regions that serve reads and pass their
writes home.

## See also

- [Why TinyStore](why.md): the engines, and why they share one directory.
- [Your first store](first-store.md): install TinyStore and run a first
  program.
