# KV

The KV engine keeps an application's current state by key in `kv.db`:
sessions that expire when idle, one-time codes, flags, the answers of
requests sent twice, counters and limits. A bucket holds values of one type;
counters, rate limits, quotas and `once` sit beside buckets with the same
vocabulary.

```ts
const sessions = store.bucket<Session>('sessions', { idle: '30d' })
const codes = store.bucket<number>('sign-in-codes', { ttl: '15m' })
const messages = store.quota('messages', { session: '100/5h', weekly: '300/7d' })

await sessions.under(userId).set(digest(token), { userId, device: 'web' })
const userId = await codes.take(digest(code)) // read and delete in one write
const used = await messages.allow(userId) // { ok, left, retryAt, windows }
```

```python
sessions = store.bucket("sessions", Session, idle="30d")
codes = store.bucket("sign-in-codes", int, ttl="15m")
messages = store.quota("messages", session="100/5h", weekly="300/7d")

await sessions.under(user_id).set(digest(token), Session(user_id=user_id, device="web"))
user_id = await codes.take(digest(code))  # read and delete in one write
used = await messages.allow(user_id)  # Allowance(ok, left, retry_at, windows)
```

```go
sessions, err := kv.OpenBucket[Session](store, "sessions", kv.Idle(30*24*time.Hour))
codes, err := kv.OpenBucket[int64](store, "sign-in-codes", kv.TTL(15*time.Minute))
messages, err := kv.OpenQuota(store, "messages",
	kv.Window("session", 100, 5*time.Hour), kv.Window("weekly", 300, 7*24*time.Hour))

err = sessions.Under(userID).Set(ctx, digest(token), Session{UserID: userID, Device: "web"})
userID, found, err := codes.Take(ctx, digest(code)) // read and delete in one write
used, err := messages.Allow(ctx, strconv.FormatInt(userID, 10))
```

```rust
let sessions = store.bucket::<Session>("sessions").idle(Duration::from_hours(24 * 30)).open()?;
let codes = store.bucket::<i64>("sign-in-codes").ttl(Duration::from_mins(15)).open()?;
let messages = store
    .quota("messages")
    .window("session", 100, Duration::from_hours(5))
    .window("weekly", 300, Duration::from_hours(24 * 7))
    .open()?;

sessions.under(user_id).set(digest(&token), &Session { user_id, device: "web".into() })?;
let user_id = codes.take(digest(&code))?; // read and delete in one write
let used = messages.allow(user_id)?;
```

## What it guarantees

- A value comes back bit for bit, as the type it was written as, or fails
  `corrupt`; every language reads the others' rows (contracts KV-01, KV-02).
  A Go `uint64` or `uint` is kept as Rust's `u64`, its whole range, and a Go
  `float32` as Rust's `f32`. Rows that Go 0.1.0 wrote for them still read.
- An expired or cleared key is absent to every call at once, with no cleanup
  job (contract KV-03).
- A version changes on every write and a write with `ifVersion` fails
  `conflict` if the key changed since it was read.
- `take`, `create` and `add` decide in one write: of two callers, one wins.
- `once` runs a key's function once while its answer is kept, and callers in
  any process wait for it (contract KV-11).

## Async Rust

With the `async` feature, `tinystore::aio` has the same buckets, counters,
rate limits, quotas and `once`, and you await their calls. Only Rust has this
choice: in Bun, Node and Python every call is already awaited, and in Go a
call blocks its goroutine.

```rust
use std::time::Duration;
use tinystore::Options;
use tinystore::aio::Store;

let store = Store::open("data", Options::default())?;
let sessions = store.bucket::<Session>("sessions").idle(Duration::from_hours(24 * 30)).open()?;
let codes = store.bucket::<i64>("sign-in-codes").ttl(Duration::from_mins(15)).open()?;

sessions.under(user_id).set(digest(&token), &Session { user_id, device: "web".into() }).await?;
let user_id = codes.take(digest(&code)).await?; // read and delete in one write
sessions.under(user_id).clear().await?; // signed out everywhere
```

You open a handle from `aio::Store` with the same builder as in the blocking
API. Opening is a plain call that you make once at start. Every call that
reads, writes or waits is the blocking call with `.await`:

| Blocking                               | Async                                        |
|----------------------------------------|----------------------------------------------|
| `sessions.set(&key, &session)?`        | `sessions.set(&key, &session).await?`        |
| `sessions.get(&key)?`                  | `sessions.get(&key).await?`                  |
| `codes.take(&code)?`                   | `codes.take(&code).await?`                   |
| `sessions.key(&key).ttl(day).set(&s)?` | `sessions.key(&key).ttl(day).set(&s).await?` |
| `targets.list(100, None)?`             | `targets.list(100, None).await?`             |
| `for entry in sessions.all()`          | `while let Some(entry) = all.next().await`   |
| `attempts.add(&ip, 1)?`                | `attempts.add(&ip, 1).await?`                |
| `api.allow(&user_id)?`                 | `api.allow(&user_id).await?`                 |
| `charges.run(&key, work)?`             | `charges.run(&key, work).await?`             |

`under` and `key`, and the options of a key's call, touch no file, so they
stay plain calls. The answers are the same types: `Entry`, `Page`, `Version`
and `Allowance`. `all` is a `Stream`, so `next` comes from
`futures::StreamExt` or `tokio_stream::StreamExt`, as
[Branches](branches.md#async-rust) shows.

TinyStore never commits, syncs the disk or waits on the thread that polls a
future (contract STORE-10):

| A call                                                                | Where it runs                                                        | If you drop its future after it was polled              |
|-----------------------------------------------------------------------|----------------------------------------------------------------------|---------------------------------------------------------|
| a write: `set`, `create`, `add`, `take`, `delete`, `clear`            | queued at the writer of `kv.db` in the first poll                    | it commits all the same                                 |
| a key's read: `get`, `entry`, `has`, `peek`                           | in the first poll when a reader is idle, else on the store's threads | it was answered in that poll, or it stops like any read |
| a step in memory: a rate limit's `allow`, an `add` with `flush_every` | in the first poll, else on the store's threads                       | it applies all the same                                 |
| a page: `list`, each page of `all`                                    | on the store's threads                                               | it stops and gives its reader back                      |
| a `once` run that waits for the run under way                         | a waiter in the engine, on no thread                                 | it stops waiting, and nothing else changes              |

A write that you never poll writes nothing. Writes that you join in one task,
for example with `tokio::try_join!`, are queued together and can share one
commit. Each of them still fails alone.

On a node that copies `kv.db` from its home, writes and steps in memory pass
on to the home, and a `once` run claims its key there. Each of them waits for
the home's answer on a thread that TinyStore starts for the call, not on the
store's threads. An `add` of counters without `flush_every` is the exception:
the node queues it and answers at once (contract REPL-12).

`aio::kv::Bucket::from(bucket)` turns a blocking bucket into an async one, and
`bucket.blocking()` gives the blocking bucket back: both are the same bucket.
Counters, rate limits, quotas and `once` cross the same way. The
[Rust with async](../../handbook/start/rust-async.md) chapter explains the
store's side.

## In these guides

| Page                              | Covers                                                   |
|-----------------------------------|----------------------------------------------------------|
| [Expiry](expiry.md)               | `ttl`, `idle`, `expiresAt`, `expire`                     |
| [Versions](versions.md)           | `entry`, `ifVersion`, safe read-modify-write             |
| [Branches](branches.md)           | `list`, `all` and `under`: keys listed, a page's entries |
| [Encrypted buckets](encrypted.md) | values sealed with the store's key                       |
| [Counters](counters.md)           | totals by key, in memory or written every add            |
| [Rate limits](rate-limits.md)     | a smooth rate with a burst, `allow`, `peek`, `reset`     |
| [Quotas](quotas.md)               | uses in windows that reset, `refund`                     |
| [Once](once.md)                   | requests sent twice, and memos                           |
| [Transactions](transactions.md)   | `store.tx`: reads that decide what to write              |

New to KV? [Sessions and limits](../../handbook/chat/sessions-and-limits.md)
in the Handbook signs users in with codes, sessions, a rate limit, a quota and
`once`.

## Reference

[Bun and Node](../../reference/bun/kv.md) ·
[Python](../../reference/python/kv.md) ·
[Go](../../reference/go/kv.md) ·
[Rust](../../reference/rust/kv.md)
