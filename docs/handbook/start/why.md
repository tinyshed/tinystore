# Why TinyStore

TinyStore is the storage of one application in one directory: SQL databases,
key-value state, background jobs, files, search, logs, metrics and more, each
an engine of the same runtime. This chapter says what it replaces, what it is
not, and why it is built the way it is.

## The stack it replaces

A web application on one server usually grows a stack of services around it:

| The application needs      | It usually runs                  | In TinyStore |
|----------------------------|----------------------------------|--------------|
| tables and queries         | PostgreSQL or MySQL              | SQL          |
| sessions, counters, limits | Redis                            | KV           |
| work in the background     | a queue service and its workers  | Jobs         |
| uploaded files             | S3 or a folder                   | Blobs        |
| search by words or meaning | Elasticsearch, a vector database | Search       |
| messages between processes | Redis pub/sub                    | Channels     |
| one worker at a time       | Redis locks                      | Locks        |
| settings                   | files and environment variables  | Config       |
| logs and events            | Loki or Elasticsearch            | Records      |
| response times and counts  | Prometheus                       | Metrics      |

Each of those services is a process to install, configure, secure, monitor,
upgrade and back up. Each one fails in its own way, and the application has
to handle every one of those failures. Most applications never need the scale
these services were built for. They pay for it anyway.

TinyStore replaces the stack with a directory that belongs to the
application, with a file per engine: [What is in the
directory](directory.md#what-is-in-the-directory) lists them. You open the
whole directory with one call, and every engine opens from it:

```ts
import { open } from '@tinyshed/tinystore'

await using store = await open('./data')

const sessions = store.bucket<Session>('sessions', { idle: '30d' })
const emails = store.queue<Email>('emails')
```

```python
import tinystore

async with await tinystore.open("data") as store:
    sessions = store.bucket("sessions", Session, idle="30d")
    emails = store.queue("emails", Email)
```

```go
store, err := tinystore.Open(ctx, "data")
if err != nil {
	return err
}
defer store.Close()

sessions, err := kv.OpenBucket[Session](store, "sessions", kv.Idle(30*24*time.Hour))
emails, err := jobs.OpenQueue[Email](store, "emails")
```

```rust
let store = Store::open("data", Options::default())?;

let sessions = store.bucket::<Session>("sessions").idle(Duration::from_hours(24 * 30)).open()?;
let emails = store.queue::<Email>("emails").open()?;
```

There is no server to install. To back up the application, you back up the
directory. To run a test, you open a store in a temporary directory and
delete it afterwards.

## The engines

| Engine   | What it keeps                                                                 | Its file        |
|----------|-------------------------------------------------------------------------------|-----------------|
| SQL      | your own SQLite databases, with migrations, a query builder and transactions  | `sql/<name>.db` |
| KV       | values by key with expiry and versions; counters, rate limits, quotas, `once` | `kv.db`         |
| Jobs     | work to run now, later or on a schedule, with retries and limits              | `jobs.db`       |
| Blobs    | files by path, written whole or not at all, checked when read                 | `blobs/`        |
| Search   | indexes of documents by their words, by vectors, or both                      | `search.db`     |
| Channels | messages to whoever listens right now                                         | memory          |
| Locks    | locks and semaphores between the parts of an application                      | memory          |
| Config   | the program's settings from files and the environment, some changed live      | `kv.db`         |
| Records  | logs and events, read back by time, level, fields and text                    | `records.db`    |
| Metrics  | counters, gauges and timers, read back exactly                                | `metrics.db`    |
| Backup   | the whole store as one checked zip, taken while it runs                       | a zip file      |

Every engine speaks the same small vocabulary. A bucket, a queue and a stream
open from the store by their name. A call that writes returns after the write
is on disk. An error has one of the same few kinds in every engine and every
language: `not found`, `conflict`, `limit`, `invalid` and a few more.

## What TinyStore is not

- **It is not a database server for many machines.** One process holds a
  directory at a time. Other processes on the same machine share it through
  that process, and other machines reach it over the network. Several
  machines can keep copies of a store, but each file is written on one
  machine at a time. If many machines must write the same rows at once, use
  a database server.
- **It is not faster than a specialised database at that database's job.**
  PostgreSQL plans complex queries better, and Redis on its own answers a
  single key faster than a write that waits for the disk. TinyStore makes the
  whole workload of one application cheap, and it removes the network hops
  between the services. [Benchmarks](../production/benchmarks.md) measures
  where it wins and where it loses.
- **It is young.** The first release came out in October 2026. Until `1.0`,
  a minor release may still change the API, and [its
  notes](../../releases/README.md) say what changed.

## Why this design?

**Why one directory.** The state of an application is more than its tables.
A session, a job waiting to run, an uploaded avatar and yesterday's logs are
all state. When each lives in a different service, a backup is several
backups taken at different moments, and a restore never quite matches. With
one directory, a backup is one zip of one moment, and moving the application
to another server is copying a folder.

**Why SQLite under every engine.** SQLite is the most tested storage engine
there is, and its file format survives crashes and power loss. It needs no
server. TinyStore runs it in a specific way: one writer per file, writes from
many callers grouped into one disk sync, and readers that never block the
writer. [Under the hood](../../README.md#under-the-hood) explains each choice.

**Why a file per engine.** A burst of log lines never makes a sign-in wait,
and each engine keeps the durability it needs: see [A file per
engine](directory.md#a-file-per-engine).

**Why one core in Rust.** A feature is written once and behaves the same in
every SDK. The first versions copied parts of a Go engine into each SDK, and
the copies drifted: see [One core, four
languages](../inside/one-core.md#why-this-design).

## See also

- [Your first store](first-store.md): install TinyStore and run a first
  program.
- [One directory, many engines](directory.md): what is in the directory, and
  how each language reaches it.
