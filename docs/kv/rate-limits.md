# Rate limits

A rate limiter allows a number of requests per time for each key, such as 100
requests per second per API key, with short bursts on top. It is part of the
KV engine. Checks run in memory and never wait for the disk, so you can call
the limiter on every request.

## Limit requests

```ts
const api = store.kv.limiter('api', { rate: '100/s', burst: 20 })

const { ok, left, retryAfter } = await api.allow(apiKey)
if (!ok) {
	return new Response('Too many requests', {
		status: 429,
		headers: { 'Retry-After': `${Math.ceil(retryAfter / 1000)}` },
	})
}
```

```python
api = store.kv.limiter("api", rate="100/s", burst=20)

ok, left, retry_after = await api.allow(api_key)
if not ok:
    raise HTTPException(429, headers={"Retry-After": str(math.ceil(retry_after))})
```

```go
api, err := kv.OpenLimiter(ctx, state, "api", kv.Rate(100, time.Second), kv.Burst(20))

allowed, err := api.Allow(ctx, apiKey)
if err == nil && !allowed.OK {
	w.Header().Set("Retry-After", strconv.Itoa(int(math.Ceil(allowed.RetryAfter.Seconds()))))
	http.Error(w, "Too many requests", http.StatusTooManyRequests)
	return
}
```

`allow` answers three things:

|                                             | Bun                        | Python                 | Go                              |
|---------------------------------------------|----------------------------|------------------------|---------------------------------|
| whether the request is allowed              | `ok`                       | `ok`                   | `OK`                            |
| how many more requests would be allowed now | `left`                     | `left`                 | `Left`                          |
| how long until the next request is allowed  | `retryAfter`, milliseconds | `retry_after`, seconds | `RetryAfter`, a `time.Duration` |

A rejected request is a normal answer, not an error.

## Rate and burst

The rate is a count per time: `'100/s'`, `'20/m'` or `'1000/5m'`. In Go, it is
`kv.Rate(100, time.Second)`. The burst is how many requests may arrive at once
before the rate applies. Without a burst, it equals the count of the rate.

TinyStore uses the generic cell rate algorithm (GCRA). For each key, it stores
only the time when the key's next request is due. Fixed time windows let up to
twice the rate through at the edge between two windows. GCRA doesn't have that
problem.

## Several requests at once

```ts
const { ok } = await api.allow(apiKey, 10) // all 10 requests, or none
```

```python
ok, left, retry_after = await api.allow(api_key, 10)  # all 10 requests, or none
```

```go
allowed, err := api.AllowN(ctx, apiKey, 10) // all 10 requests, or none
```

More requests than the burst can never be allowed at once, so such a call
fails with an invalid argument error.

## Separate limits per tenant

```ts
const { ok } = await api.of(tenantId).allow(userId)
```

```python
ok, left, retry_after = await api.of(tenant_id).allow(user_id)
```

```go
allowed, err := api.Of(tenantID).Allow(ctx, userID)
```

`of` gives each tenant its own keys, so the same user id counts separately in
each tenant.

## What a crash can do

The limiter keeps its state in memory and writes it to disk every second. If
the process crashes, it forgets up to one second of requests, so up to one
extra burst can get through after a restart. That is fine for protecting a
server from load. For limits that a plan promises, such as messages per week,
use a [quota](quotas.md), which writes every use to disk.

A key that has had no requests for a while is forgotten automatically. You
don't need to clean up keys.

## Requests from many processes

All processes that share a store also share its limiters, because the
limiter runs in the process that owns the store. Ten web workers that open the
same directory through the sidecar see one limit, not ten.

## See also

- [Quotas](quotas.md): durable limits over several windows.
- [Counters](counters.md): count events instead of limiting them.
- [kv/README.md](../../kv/README.md#limiter): the full contract of the
  limiter.
