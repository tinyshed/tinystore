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
- On a node that copies the file from its home, an add answers at once and is
  counted once at the home, however often it is sent (contract REPL-12).

## See also

- [KV](README.md): the engine's overview.
- [Rate limits](rate-limits.md) and [Quotas](quotas.md): limits built on
  counting.
- [Metrics](../metrics/README.md): counters you chart over time.
