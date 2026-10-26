# Rate limits

A rate limit lets a key through at a steady rate, with a burst on top: 100
requests a second per user, 5 sign-in guesses a minute per IP. There is no
window edge that lets twice the rate through, and `allow` answers instead of
throwing, with the time the next request may try again.

```ts
const api = store.rateLimit('api', { rate: '100/s', burst: 20 })

const { ok, left, retryAt } = await api.allow(userId)
if (!ok) return new Response('slow down', { status: 429, headers: { 'retry-after': secondsUntil(retryAt!) } })
```

```python
api = store.rate_limit("api", rate="100/s", burst=20)

answer = await api.allow(user_id)  # Allowance(ok, left, retry_at, windows)
if not answer.ok:
    return Response("slow down", status_code=429, headers={"retry-after": seconds_until(answer.retry_at)})
```

```go
api, err := kv.OpenRateLimit(store, "api", kv.Rate(100, time.Second), kv.Burst(20))

answer, err := api.Allow(ctx, userID) // kv.Allowance{OK, Left, RetryAt}
if err == nil && !answer.OK {
	w.Header().Set("Retry-After", secondsUntil(answer.RetryAt))
	w.WriteHeader(http.StatusTooManyRequests)
}
```

```rust
let api = store.rate_limit("api").rate(100, Duration::from_secs(1)).burst(20).open()?;

let answer = api.allow(&user_id)?; // Allowance { ok, left, retry_at, .. }
if !answer.ok {
    return Ok(too_many_requests(answer.retry_at));
}
```

## The calls

| Call            | Does                                                          |
|-----------------|---------------------------------------------------------------|
| `allow(key)`    | counts one request and says whether it passes                 |
| `allow(key, n)` | counts `n` at once, all of them or none                       |
| `peek(key)`     | the answer one request would get, counting nothing            |
| `reset(key)`    | forgets the key, so its next request starts with a full burst |

`allow` returns `ok`, how many more would pass now as `left`, and when a
refused request may try again as `retryAt / retry_at / RetryAt / retry_at`.

## How it behaves

- `rate` is the steady rate, as `'100/s'`, `'5/m'` or `'1000/h'`; `burst` is
  how many may pass at once.
- More requests at once than the burst never pass, and are `invalid`.
- The limit's times live in memory and reach `kv.db` every second. A crash
  can let at most one burst more through, which is the trade for a check that
  costs no disk write.

## Rate limit or quota

| You want                                     | Use                                        |
|----------------------------------------------|--------------------------------------------|
| traffic smoothed: 100 a second, 20 at once   | a rate limit, `rate: '100/s'`, `burst: 20` |
| a count a plan sells: 100 messages in 5 h    | a quota, `session: '100/5h'`               |
| at most 5 in any 60 s, counted back from now | neither: no sliding window is built        |

- A rate limit has no window: a key earns back one use every `1/rate`, up to
  `burst`. After a crash it may let one burst more through.
- A quota's window starts at a key's first use and resets a span later, and
  it is written on every `allow`, so a crash hands out no use.
- A quota of `'5/1m'` is the nearest to "5 a minute": 5 pass in each window,
  but a window's last uses and the next one's first pass within seconds. One
  use at 0 s, four at 58 s and five at 61 s all pass: nine in three seconds.

## Async Rust

With the `async` feature, `aio`'s rate limits decide the same way, and you
await each call. In Bun, Node and Python every call is already awaited, and
in Go a call blocks its goroutine, so this section shows only Rust.

```rust
use std::time::Duration;
use tinystore::Options;
use tinystore::aio::Store;

let store = Store::open("data", Options::default())?;
let api = store.rate_limit("api").rate(100, Duration::from_secs(1)).burst(20).open()?;

let answer = api.allow(&user_id).await?; // decided in memory, in its first poll
if !answer.ok {
    return Ok(too_many_requests(answer.retry_at));
}
```

A rate limit keeps its times in memory, so `allow`, `allow_n`, `peek` and
`reset` answer in their future's first poll. When memory does not hold a key
yet, the call reads it from `kv.db` first, on the store's threads if the read
would wait. Once its future was polled, an `allow` or a `reset` takes effect
even if you drop the future (contract DUR-11).

On a node that copies `kv.db`, `allow`, `allow_n` and `reset` ask the file's
home and wait for its answer on a thread that TinyStore starts for the call.
`peek` reads the node's copy. Either way, the thread that polls the call
never waits.

## See also

- [KV](README.md): the engine's overview.
- [Quotas](quotas.md): uses per plan, in windows that reset.
- [Sign in with a code](../../handbook/chat/sessions-and-limits.md#sign-in-with-a-code):
  a rate limit on guesses.
