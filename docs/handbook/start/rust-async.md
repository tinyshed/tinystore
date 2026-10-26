# Rust with async

A Rust program can await TinyStore instead of blocking a thread on it. The
`async` feature adds `tinystore::aio`, whose futures run on tokio,
async-std, smol or any other executor, because TinyStore does not bring a
runtime of its own. Every engine is there: each call that reads, writes or
waits returns a future, and you run a transaction and close the store with
`.await` too.

> [!NOTE]
> **Rust only**
> This page is about Rust. In Bun, Node and Python every call is already
> awaited, and in Go a call blocks its goroutine, which costs little. The
> examples on this page show only Rust for that reason.

## Turn it on

```toml
[dependencies]
tinystore = { version = "0.3", features = ["async"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

```rust
use tinystore::Options;
use tinystore::aio::Store;

#[tokio::main]
async fn main() -> tinystore::Result<()> {
    let store = Store::open("data", Options::default())?; // a plain call, made once at start
    let stock = store.bucket::<i64>("stock").open()?; // so is opening a handle

    stock.set("sku-1", &10).await?;
    assert_eq!(stock.get("sku-1").await?, Some(10));

    let taking = stock.clone(); // the closure owns its handle
    let left = store
        .tx(move |tx| -> tinystore::Result<i64> {
            let left = tx.with(&taking).get("sku-1")?.unwrap_or(10);
            tx.with(&taking).set("sku-1", &(left - 1))?;
            Ok(left - 1)
        })
        .await?; // 9: the transaction ran on the store's own threads
    println!("{left} left");

    store.close().await // closes on a thread of its own
}
```

`aio::Store` opens as `tinystore::Store` does. Opening is a plain call that
you make once at start, because a first open may migrate a file. Every
engine's handle opens from `aio::Store` with the same builder as in the
blocking API, and you await its calls. A bucket, a queue or a table's search
index kept in a database opens from the async database the same way. Each
engine's guide lists its awaited calls, and [the table at the
end](#each-engines-calls) links them.

Every future is `Send + 'static`, so you can pass it to `tokio::spawn`,
except those that borrow what you give them. `run` of `once` is `'static`
only when the key, the function and the function's future are, and a lock's
or a semaphore's `hold` and `try_hold` only when their function owns what it
uses. A job run's `step` borrows the run, and an upload's `write` and a
file's `read_at` borrow their bytes: you await those in place. A future does
nothing until it is first polled.

## Where the work runs

| A call                                                           | Where it runs                                                                                        | If its future is dropped after it was polled            |
|------------------------------------------------------------------|------------------------------------------------------------------------------------------------------|---------------------------------------------------------|
| a write: `bucket.set(...)`, `queue.add(...)`, `db.exec(...)`     | queued at its file's writer in the first poll, its commit on the store's threads                     | it commits all the same                                 |
| a point read: `bucket.get(...)`, a file's `head`                 | in the first poll when a reader is idle, else on the store's threads                                 | it was answered in that poll, or it stops like any read |
| a page or a query: `bucket.list(...)`, `db.all(...)`, a search   | on the store's threads                                                                               | it stops and gives its reader back                      |
| a wait: a `once` run that waits for another, a tail's next batch | a waiter in the engine, on no thread                                                                 | it stops waiting, and nothing else changes              |
| a wait for an index: `index.ready()`                             | a waiter the index wakes, and a look at its status on the store's threads each time and every 100 ms | it stops waiting, and the rebuild goes on               |
| a hold: `lock.hold(...)`, `semaphore.hold(...)`                  | a place in the key's line that blocks no thread, then its function in the future                     | it leaves the line, or lets the key go                  |
| a stream: `channel.each()`, `config.watch()`, `bucket.all()`     | woken by each message or change, or a page at a time on the store's threads                          | it stops                                                |
| a worker: `queue.work(...)`                                      | its handlers inside its future, its claims on a thread of its own                                    | it stops at once, and the jobs it ran run again         |
| a transaction: `store.tx(...)`, `db.tx(...)`                     | its closure, blocking, on the store's threads                                                        | it runs to its end                                      |
| a metrics write or a backup: `stats.ingest(...)`                 | blocking, on the store's threads                                                                     | it runs to its end                                      |
| a plain call: `config.get()`, `channel.publish(&message)`        | in memory, not awaited                                                                               | —                                                       |
| closing: `store.close()`                                         | on a thread that the call starts                                                                     | the store closes all the same                           |

No call commits, syncs the disk or waits on the thread that polls its
future (contract STORE-10). So your executor's threads never wait for the
disk. A point read that would wait in the poll, for a reader or for a
write, runs again on the store's threads instead.

On a node that copies a file from its home, a write passes on to the home,
and a `once` run claims its key there. Each of them waits for the home's
answer on a thread that TinyStore starts for the call, not on the store's
threads. So a slow home, or a run under way at the home, never holds the
threads that your other calls need.

Once you poll a write, it commits even if you drop its future (contract
DUR-11). A write whose future you never poll writes nothing. A read whose
future you drop stops where it is and gives its reader back (contract
STORE-09), so `tokio::time::timeout` around a read stops it.

The store's threads are a pool of at most sixteen, and each running worker
adds a thread of its own beside them for its claims. TinyStore starts them as
calls need them, ends each one after a minute without work, and stops all of
them when the store closes. If all sixteen are busy when a commit needs a
thread, the commit gets a thread of its own. This matters because the busy
threads may be waiting for the memory that the commit gives back.

## What dropping a future does

A future that is never polled reads and writes nothing. After its first poll:

- **A read stops.** Its SQLite statement is interrupted and its reader goes
  back at once, as if a `CancellationToken` was cancelled (contract
  STORE-09). This is what a web handler wants when its client goes away.
  `tokio::time::timeout(duration, call)` stops a read this way when the time
  runs out.
- **A write commits.** Writes share a commit, and a commit cannot stop half
  way, so a write that was polled commits whether or not anyone still awaits
  it (contract DUR-11). Only its answer is lost. To know that a write
  happened, await it.
- **A transaction runs to its end**, as a write does.

A `CancellationToken` still works around a call. Make the future inside
`token.run(...)`, and its read stops when you call `token.cancel()`: it fails
with a cancelled error (`ErrorKind::Cancelled`).

## Transactions

```rust
let taking = stock.clone();
let placed = store
    .tx(move |tx| -> tinystore::Result<bool> {
        let left = tx.with(&taking).get("sku-1")?.unwrap_or(0);
        if left < 1 {
            return Ok(false); // sold out: nothing is written
        }
        tx.with(&taking).set("sku-1", &(left - 1))?;
        Ok(true)
    })
    .await?;
```

This block uses the `store` and `stock` of the first one. The closure of
`store.tx` is blocking, and it runs on one of the store's threads, never on
your executor's thread. A transaction holds its file's writer, so it must not
wait for your program's other tasks. Inside the closure, the calls go through
`tx.with(&handle)`, exactly as in the blocking API. If the closure returns an
error, the transaction rolls back and the future returns that error. If the
closure panics, the transaction rolls back and the panic continues where you
await the future.

Once its future is polled, a transaction runs to its end, even if you drop
the future. To know whether it committed, await it.

Don't call `block_on` on an awaited call inside the closure. That call would
wait for the writer the transaction holds, so it fails with an invalid error
(`ErrorKind::Invalid`) instead.

## Streams

```rust
use futures::StreamExt; // next() on a Stream

let events = store.channel::<String>("chat-events").open()?;
let mut each = events.under("chat-7").each()?; // listens from here
while let Some(message) = each.next().await {
    println!("{}", message?.value);
}
```

This block uses the `store` of the first one. A loop over a channel's
messages, a watch of a config or a queue, a walk over every key, job, file or
record and a tail of records are each a `Stream` of the `futures-core`
crate, and so is a file that Blobs' `get` returns, piece by piece. So
`next()` from `futures::StreamExt` or `tokio_stream::StreamExt` reads them,
and every item is a `Result`. A stream of messages or changes blocks no
thread while it waits, because a publish or a change wakes it, and a walk
reads one page at a time on the store's threads. A channel has no handler
form in the async API: to handle its messages in the background, spawn a
task that reads the stream.

## Blocking and async in one program

```rust
let store = Store::open("data", Options::default())?;
let tool = store.blocking(); // the same store, for blocking calls
let again = Store::from(tool.clone()); // the same store, its calls awaited
```

`Store::from` and `store.blocking()` cross between the two APIs. Both are
cheap copies of one store, its engines and its files, never a second store.
A command-line tool can use the blocking API while the server beside it uses
the async one.

## Errors and closing

An awaited call fails with the same `tinystore::Error` and `ErrorKind` as its
blocking twin. A call made after its store closed fails with a closed error
(`ErrorKind::Closed`), and so does a call that was still waiting for the
store's threads when the store closed.

There is one way to time out a call: your runtime's timeout, such as
`tokio::time::timeout`. When it drops the future, a read stops and a write
still commits.

`store.close().await` closes the store on a thread that the call starts. It
waits for background work, for the handlers that run and for each engine's
last writes, so your executor's thread waits for nothing. If you drop the
last handle instead, the store closes on the thread that drops it, which may
be your executor's thread. Await `close` instead.

## Each engine's calls

Each engine's guide shows its awaited calls in a section named Async Rust:

| Engine   | Guide                                                  | What you await                              |
|----------|--------------------------------------------------------|---------------------------------------------|
| KV       | [KV](../../guides/kv/README.md#async-rust)             | keys, counters, rate limits, quotas, `once` |
| Config   | [Config](../../guides/config/README.md#async-rust)     | a config's changes, its watch as a stream   |
| Locks    | [Locks](../../guides/locks/README.md#async-rust)       | a lock's or a semaphore's hold              |
| Channels | [Channels](../../guides/channels/README.md#async-rust) | a channel's messages as a stream            |
| Jobs     | [Jobs](../../guides/jobs/README.md#async-rust)         | adds, claims, steps, schedules, a worker    |
| SQL      | [SQL](../../guides/sql/README.md#async-rust)           | queries, writes, batches, transactions      |
| Blobs    | [Blobs](../../guides/blobs/README.md#async-rust)       | files, uploads, a file's bytes as a stream  |
| Search   | [Search](../../guides/search/README.md#async-rust)     | sets, searches, `ready`, a table's index    |
| Records  | [Records](../../guides/records/README.md#async-rust)   | appends, reads, counts, a tail as a stream  |
| Metrics  | [Metrics](../../guides/metrics/README.md#async-rust)   | ingest, queries, pages, descriptions        |
| Backup   | [Backup](../../guides/backup/README.md#async-rust)     | a backup's write                            |

## Why this design?

**Why no runtime of its own.** An async library that depends on tokio forces
tokio on every program that uses it, and one that supports several runtimes
needs a feature for each. TinyStore's work is SQLite, which is synchronous,
so a runtime would only move it to a thread. TinyStore already has its own
threads for that, so its futures wake through the standard `Waker` and any
executor runs them.

**Why a transaction's closure is blocking.** A transaction holds its file's
writer until it commits. If its closure could await other tasks, every write
to that file would wait for them, and one slow request would stall all the
others. A blocking closure on the store's threads keeps the writer for the
time the transaction's own reads and writes take.

**Why a polled write commits.** Writes that arrive together share one commit
and one sync of the disk. Once a write is queued, other writes may be
committing with it, and taking it out would mean failing them too. So a
dropped future only stops waiting for the answer. A read is different: it
changes nothing, so stopping it is always safe.

## See also

- [One directory, many engines](directory.md): the store, its files and the
  ways to reach it.
- [Store API for Rust](../../reference/rust/store.md): `aio::Store` and its
  calls.
