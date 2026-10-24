# Rust with async

A Rust program can await TinyStore instead of blocking a thread on it. The
`async` feature adds `tinystore::aio`, whose futures run on tokio,
async-std, smol or any other executor, because TinyStore does not bring a
runtime of its own. Today `aio` has the store and the KV engine: you read
and write keys, count, limit and run `once` with `.await`, and you run a
transaction and close the store the same way. The other engines' handles
don't have awaited calls yet.

> [!NOTE]
> **Rust only**
> This page is about Rust. In Bun, Node and Python every call is already
> awaited, and in Go a call blocks its goroutine, which costs little. The
> examples on this page show only Rust for that reason.

## Turn it on

```toml
[dependencies]
tinystore = { version = "0.1", features = ["async"] }
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
you make once at start, because a first open may migrate a file. A KV
handle opens from `aio::Store` with the same builder as in the blocking API,
and you await its calls. The [KV guide](../../guides/kv/README.md#async-rust)
lists them.

The other engines' handles open from `store.blocking()`, and their calls are
still blocking. Make them inside `store.tx`, which runs them on the store's
own threads, or on a thread that may block, such as one from
`tokio::task::spawn_blocking`. Don't call them directly in an `async`
function, because they would block your executor's thread.

Every future is `Send + 'static`, so you can pass it to `tokio::spawn`.
The one exception is `run` of `once`, whose future can borrow what its
function uses: it is `'static` only when the key, the function and the
function's future are. A future does nothing until it is first polled.

## Where the work runs

| A call                                      | Where it runs                                                        | If its future is dropped after it was polled            |
|---------------------------------------------|----------------------------------------------------------------------|---------------------------------------------------------|
| a write: `bucket.set(...)`                  | queued at its file's writer in the first poll                        | it commits all the same                                 |
| a point read: `bucket.get(...)`             | in the first poll when a reader is idle, else on the store's threads | it was answered in that poll, or it stops like any read |
| a page: `bucket.list(...)`                  | on the store's threads                                               | it stops and gives its reader back                      |
| a wait: a `once` run that waits for another | a waiter in the engine, on no thread                                 | it stops waiting, and nothing else changes              |
| a transaction: `store.tx(...)`              | its closure, blocking, on the store's threads                        | it runs to its end                                      |
| closing: `store.close()`                    | on a thread that the call starts                                     | the store closes all the same                           |

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

A `set` makes the words of its documents in the first poll. When the
documents add up to more than 4 KiB of JSON, for example in a `set_many` of
many documents, it makes them on the store's threads instead. `ready` holds no thread while it
waits. The index wakes it when its rebuild switches in or fails, and `ready`
also checks the index's status on the store's threads every 100 ms. On a
store opened without background work, `ready` runs the maintenance itself,
on the store's threads.

The store's threads are a pool of at most sixteen. TinyStore starts them as
calls need them, ends each one after a minute without work, and stops all of
them when the store closes.

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
messages and a watch of a config are each a `Stream` of the `futures-core`
crate. So `next()` from `futures::StreamExt` or `tokio_stream::StreamExt`
reads them, and every item is a `Result`. A stream blocks no thread while it
waits, because a publish or a change wakes it. The async API has no handler
form: to handle messages in the background, spawn a task that reads the
stream.

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

`store.close().await` closes the store on a thread that the call starts. It
waits for background work, for the handlers that run and for each engine's
last writes, so your executor's thread waits for nothing. If you drop the
last handle instead, the store closes on the thread that drops it, which may
be your executor's thread. Await `close` instead.

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

## See also

- [One directory, many engines](directory.md): the store, its files and the
  ways to reach it.
- [Store API for Rust](../../reference/rust/store.md): `aio::Store` and its
  calls.
- [KV](../../guides/kv/README.md#async-rust): each KV call, blocking and
  awaited.
