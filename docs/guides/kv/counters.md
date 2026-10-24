# Counters

Counters are numbers by key that only add up: sign-in attempts per IP, page
views, uses of a feature. An add returns the new count, a counter can expire
like any key, and a counter that may lose its last second to a crash can be
kept in memory and cost no commit at all.

```ts
const attempts = store.counters('login-attempts', { ttl: '15m' })
const views = store.counters('page-views', { flushEvery: '1s' })

const n = await attempts.add(clientIp, 1) // 1, 2, 3…
await attempts.get(clientIp) // the count, 0 when there is none
await views.add('/notes', 1)
```

```python
attempts = store.counters("login-attempts", ttl="15m")
views = store.counters("page-views", flush_every="1s")

n = await attempts.add(client_ip, 1)  # 1, 2, 3…
await attempts.get(client_ip)  # the count, 0 when there is none
await views.add("/notes", 1)
```

```go
attempts, err := kv.OpenCounters(store, "login-attempts", kv.TTL(15*time.Minute))
views, err := kv.OpenCounters(store, "page-views", kv.FlushEvery(time.Second))

n, err := attempts.Add(ctx, clientIP, 1) // 1, 2, 3…
count, err := attempts.Get(ctx, clientIP)
_, err = views.Add(ctx, "/notes", 1)
```

```rust
let attempts = store.counters("login-attempts").ttl(Duration::from_mins(15)).open()?;
let views = store.counters("page-views").flush_every(Duration::from_secs(1)).open()?;

let n: i64 = attempts.add(&client_ip, 1)?; // 1, 2, 3…
views.add("/notes", 1)?;
```

## Written each add, or kept in memory

| Counter            | An add                                   | A crash loses       | Joins a transaction |
|--------------------|------------------------------------------|---------------------|---------------------|
| default            | a durable write, shared with others      | nothing             | yes                 |
| `flushEvery: '1s'` | a number in memory, written every second | at most that second | no                  |

`flushEvery` is `flush_every` in Python and `kv.FlushEvery` in Go.

## How counters behave

- An absent or expired counter is 0.
- A counter's `ttl` window starts with its first add and does not slide: 15
  minutes after the first attempt it starts from 0 again. A lockout that
  should not reset on a clock's edge is a [rate limit](rate-limits.md).
- A sum past what an int64 holds is `limit`, and changes nothing.
- On a node that copies the file from its home, an add of counters without
  `flushEvery` answers at once and is counted once at the home, however often
  it is sent (contract REPL-12).

## Async Rust

With the `async` feature, `aio`'s counters add up the same way, and you
await each call. In Bun, Node and Python every call is already awaited, and
in Go a call blocks its goroutine, so this section shows only Rust.

```rust
use std::time::Duration;
use tinystore::Options;
use tinystore::aio::Store;

let store = Store::open("data", Options::default())?;
let attempts = store.counters("login-attempts").ttl(Duration::from_mins(15)).open()?;
let views = store.counters("page-views").flush_every(Duration::from_secs(1)).open()?;

let n: i64 = attempts.add(&client_ip, 1).await?; // a durable write, shared with others
views.add("/notes", 1).await?; // a number in memory: answered in its first poll
```

The builder decides where a call runs, never the call:

| Counter       | `add` and `delete`                                                                   | `clear`                |
|---------------|--------------------------------------------------------------------------------------|------------------------|
| default       | queued at the writer of `kv.db` in the first poll                                    | queued at the writer   |
| `flush_every` | in memory, in the first poll, or on the store's threads when memory must flush first | on the store's threads |

`get` answers from memory, or reads `kv.db` in the first poll when a reader is
idle. Once its future was polled, an `add` or a `delete` takes effect even if
you drop the future, in memory or in `kv.db` (contract DUR-11).

On a node that copies `kv.db` from its home:

- An `add` of the default counters is queued on that node and answers at
  once, with the copy's count and the adds still queued. The home counts it
  once, however often it is sent (contract REPL-12).
- `get` reads the node's copy.
- An `add` with `flush_every`, and every `delete` and `clear`, passes on to
  the home and waits for its answer. It waits on a thread that TinyStore
  starts for the call, not on the store's threads.

## See also

- [KV](README.md): the engine's overview.
- [Rate limits](rate-limits.md) and [Quotas](quotas.md): limits built on
  counting.
- [Metrics](../metrics/README.md): counters you chart over time.
