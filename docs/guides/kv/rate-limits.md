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
refused request may try again as `retryAt` (`retry_at`, `RetryAt`).

## How it behaves

- `rate` is the steady rate, as `'100/s'`, `'5/m'` or `'1000/h'`; `burst` is
  how many may pass at once.
- More requests at once than the burst never pass, and are `invalid`.
- The limit's times live in memory and reach `kv.db` every second. A crash
  can let at most one burst more through, which is the trade for a check that
  costs no disk write.

## See also

- [KV](README.md): the engine's overview.
- [Quotas](quotas.md): uses per plan, in windows that reset.
- [Sign in with a code](../../handbook/chat/sessions-and-limits.md#sign-in-with-a-code):
  a rate limit on guesses.
