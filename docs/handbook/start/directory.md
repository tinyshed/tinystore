# One directory, many engines

A store is one directory with a file per engine, held by one process at a
time. This chapter shows what each file is, how engines share a file when you
ask them to, and the four ways a program reaches a store: in its own process,
through a sidecar, through a private child, or over the network.

## Four ways to open a store

```ts
import { connect, open } from '@tinyshed/tinystore'

const shared = await open('./data')                       // the directory's sidecar, shared with other processes
const alone = await open('./data', { private: true })     // a server for this process alone: tests and scripts
const inside = await open('./data', { embedded: true })   // the core inside this process: no sidecar
const remote = await connect('tls://db.internal:7443', { token })
```

```python
shared = await tinystore.open("data")                   # the directory's sidecar, shared with other processes
alone = await tinystore.open("data", private=True)      # a server for this process alone: tests and scripts
inside = await tinystore.open("data", embedded=True)    # the core inside this process: no sidecar
remote = await tinystore.connect("tls://db.internal:7443", token=token)
```

```go
shared, err := tinystore.Open(ctx, "data")                      // the directory's sidecar, shared with other processes
alone, err := tinystore.Open(ctx, "data", tinystore.Private())   // a server for this process alone: tests and scripts
inside, err := tinystore.Open(ctx, "data", tinystore.Embedded()) // the core inside this process: no sidecar
remote, err := tinystore.Connect(ctx, "tls://db.internal:7443", token)
```

```rust
// A Rust program always runs the core in its own process.
let store = Store::open("data", Options::default())?;
```

Each line above opens a different store, for the sake of the example. A real
program picks one of them. The calls after `open` are the same in all four
cases, because all four reach the same core:

| Way              | Where the core runs                         | Use it for                                                   |
|------------------|---------------------------------------------|--------------------------------------------------------------|
| sidecar, default | a `tinystore` process beside your program   | a web application, several processes that share a store      |
| `private`        | a child process that lives and dies with it | tests and scripts that must not touch a running sidecar      |
| `embedded`       | inside your program                         | one process that owns the store, with the fastest calls      |
| `connect`        | a `tinystore serve` on another machine      | programs that reach a store over the network, with a token   |
| Rust             | inside your program                         | Rust programs, which call the engines directly with no bytes |

## What is in the directory

```text
data/
├── LOCK              held by the one process that owns the store
├── server/SERVE      the sidecar's address, so other programs find it
├── kv.db             buckets, counters, limits, `once`, live settings
├── jobs.db           queues and schedules
├── sql/
│   └── chat.db       a database the application named, one file each
├── blobs/            uploaded files
├── search.db         search indexes
├── records.db        logs and events
├── metrics.db        metrics
├── encryption.key    the key of encrypted buckets, unless you keep it elsewhere
└── log/              a node's log of each file's commits, when the store is a node
```

A file appears when its engine first opens. A program that only uses buckets
has `kv.db` and nothing else. Every `.db` file is an SQLite database, and the
`-wal` and `-shm` files next to one belong to it. Channels and locks keep
nothing on the disk, so they have no file.

You never edit these files by hand, but you can read them. The `tinystore`
command reads a store while it runs, and any SQLite tool opens a `.db` file.

## A file per engine

Every file has its own writer. A burst of a thousand log lines goes to
`records.db`, while a sign-in writes its session to `kv.db` at the same time,
and neither waits for the other (contract STORE-02).

Each file also has its own durability, which says how far a write goes before
the call returns:

| Durability | A write returns after                 | It survives               | Default for          |
|------------|---------------------------------------|---------------------------|----------------------|
| `'full'`   | the disk has synced it                | a crash, and a power loss | kv, jobs, sql, blobs |
| `'os'`     | the operating system has it, unsynced | a crash of the program    | records, metrics     |

You can set one durability for the whole store when you open it, such as
`open('./data', { durability: 'os' })`, and a database can set its own. The
defaults suit most programs: what the application decides is synced, and what
it observes about itself is fast.

## Two engines in one file

A write is atomic within one file, and never across two. So if a request
inserts a message into `sql/chat.db` and adds a job to `jobs.db`, a crash
between the two can leave the message without its job.

| What                                                                       | Commits together                                     |
|----------------------------------------------------------------------------|------------------------------------------------------|
| buckets of `kv.db`                                                         | yes, in `store.tx`                                   |
| queues of `jobs.db`                                                        | yes, in `store.tx`                                   |
| a database's rows and the buckets, queues and table indexes opened from it | yes, in `db.tx`                                      |
| a bucket of `kv.db` and a queue of `jobs.db`                               | no: `store.tx` refuses the second file's handle      |
| a file in `blobs/`, its metadata in a bucket, its document in an index     | no: three files, each written whole on its own       |
| a backup                                                                   | one moment of each file, not one moment of the store |

Where no commit covers them, write in an order a crash can be repaired from:
the file first, then the metadata that names it, then the index. A crash
leaves at worst a file nothing names, or metadata the index misses, and a
check that compares them repairs both.

When two things must commit together, open one inside the other's file. A
bucket or a queue opened from a database lives in that database's file, and
commits with its rows:

```ts
const db = await store.database('chat', { migrations: './migrations' })
const replies = db.queue<Reply>('replies') // kept in sql/chat.db, not in jobs.db
```

```python
db = await store.database("chat", migrations="migrations")
replies = db.queue("replies", Reply)  # kept in sql/chat.db, not in jobs.db
```

```go
db, err := sqldb.Open(ctx, store, "chat", sqldb.Migrations(os.DirFS("migrations")))
replies, err := jobs.OpenQueue[Reply](db, "replies") // kept in sql/chat.db, not in jobs.db
```

```rust
let db = store.database("chat").migrations("migrations").open()?;
let replies = db.queue::<Reply>("replies").open()?; // kept in sql/chat.db, not in jobs.db
```

`replies` is a queue like any other, and a worker runs its jobs the same way.
The difference is where it lives: a transaction of `db` can now insert a
message and add its reply job in one commit (contract DUR-06). Chapter 7,
Replies in the background, builds exactly that.

## Close the store

```ts
await store.close()
```

```python
await store.close()
```

```go
err := store.Close()
```

```rust
store.close()?;
```

`close` stops the workers, waits for the handlers that are running, writes
what the loggers and metrics still hold, and closes the engines in the
reverse order of opening. A store whose last handle is dropped closes the same
way. A handle used after its store closed fails with a `closed` error
(`ClosedError` in TypeScript and Python, `tinystore.ErrClosed` in Go,
`ErrorKind::Closed` in Rust).

In TypeScript, `await using store = await open(…)` closes the store at the
end of the block, and `async with` does the same in Python.

## Why this design?

**Why one owner per directory.** SQLite allows several processes to write
one file. They then take turns through a lock on the file, and under load a
process that waits too long gets `database is locked`. Writes from different
processes also cannot share a disk sync, because no process sees the others'
writes before they commit. With one owner, every write to a file goes through
one queue in one process: waiting is a short, orderly line, and a hundred
writes share a few syncs. The `LOCK` file makes the rule hold: a second
process that tries to own the directory is refused (contract STORE-01).

**Why the sidecar uses the same protocol as a remote server.** A Bun, Python
or Go call becomes a message of the wire protocol, whether it travels
through memory to an embedded core, through a socket to a sidecar, or over
TLS to another machine. Only the transport changes. So a program moves from
one way to another by changing its `open` line, and a test that runs against
a private child tests the same code paths as production. Chapter 22, One core,
four languages, shows the protocol in detail.

**Why a file per engine instead of one big database.** Logs and metrics
arrive in bursts and can lose their last second to a power loss. Sessions
and jobs arrive steadily and must never lose anything. In one file, they
would have to share one writer and one durability, so either logs would slow
down sign-ins or sessions would be less safe. Separate files let each engine
keep the right trade-off, and let you choose to join two of them when a
commit must cover both.

## See also

- [Your first store](first-store.md): a first program, and the files it
  makes.
- [The chat we build](chat.md): the application the rest of the book builds.
