# Counters

A counter bucket stores one integer per key, which you increase with `add`.
Use counters for page views, sign-in attempts and anything else you count.
Counters can keep their numbers in memory and write them to disk every
second, which makes counting nearly free.

## Count

```ts
const views = store.kv.counters('page-views')

await views.add('/pricing')      // 1
await views.add('/pricing')      // 2
await views.add('/pricing', 10)  // 12
await views.get('/pricing')      // 12
await views.get('/never-seen')   // 0
```

```python
views = store.kv.counters("page-views")

await views.add("/pricing")      # 1
await views.add("/pricing")      # 2
await views.add("/pricing", 10)  # 12
await views.get("/pricing")      # 12
await views.get("/never-seen")   # 0
```

```go
views, err := kv.OpenCounters(ctx, state, "page-views")

n, err := views.Add(ctx, "/pricing", 1)  // 1
n, err = views.Add(ctx, "/pricing", 1)   // 2
n, err = views.Add(ctx, "/pricing", 10)  // 12
n, err = views.Get(ctx, "/pricing")      // 12
n, err = views.Get(ctx, "/never-seen")   // 0
```

`add` returns the new value. A key that doesn't exist counts as zero. A
counter is a 64-bit integer. In Bun, open the bucket with
`counters(name, 'bigint')` to get `bigint` values instead of numbers.

`max(key, n)` keeps the larger of the stored value and `n`, for example the
highest score of a player. `delete` resets a key, and `clear` resets a whole
branch.

## Limit sign-in attempts

```ts
const attempts = store.kv.counters('sign-in-attempts', { defaultTtl: '15m', loseAtMost: '1s' })

const byIp = await attempts.of('ip').add(clientIp)
const byEmail = await attempts.of('email').add(form.email)
if (byIp > 20 || byEmail > 5) {
	return tooManyAttempts()
}
```

```python
attempts = store.kv.counters("sign-in-attempts", default_ttl="15m", lose_at_most="1s")

by_ip = await attempts.of("ip").add(client_ip)
by_email = await attempts.of("email").add(form.email)
if by_ip > 20 or by_email > 5:
    return too_many_attempts()
```

```go
attempts, err := kv.OpenCounters(ctx, state, "sign-in-attempts",
	kv.DefaultTTL(15*time.Minute), kv.LoseAtMost(time.Second))

byIP, err := attempts.Of("ip").Add(ctx, clientIP, 1)
byEmail, err := attempts.Of("email").Add(ctx, form.Email, 1)
if byIP > 20 || byEmail > 5 {
	return errTooManyAttempts
}
```

With a default TTL, a counter starts its window at the first `add` and resets
to zero when the window ends. Later `add` calls don't extend the window, so an
attacker can't keep a counter alive by trying again.

## Count in memory

`loseAtMost` (`LoseAtMost`, `lose_at_most`) keeps counters in memory and
writes the changes to disk at that interval, and when the store closes. `add`
then doesn't wait for the disk at all, which matters for counters that change
on every request.

The trade-off is in the name: if the process crashes, the changes since the
last write are lost. For sign-in attempts or view counts, losing a second is
fine. For money, it isn't, so use normal counters there.

A few rules keep this safe:

- Memory holds at most 100,000 counters of a bucket. When it is full, the next
  new key waits for a write to disk.
- All handles of one counter bucket must use the same `loseAtMost`. Opening it
  again with another interval, or without one, fails.
- A counter kept in memory can't be part of a [transaction](transactions.md).

## Overflow

A counter never wraps around. An `add` that would go past the range of a
64-bit integer fails with a limit error (`ErrLimit`, `LimitError`) and changes
nothing.

## Limits and defaults

|                                       |                                             |
|---------------------------------------|---------------------------------------------|
| Value                                 | a 64-bit signed integer                     |
| Counters in memory, with `loseAtMost` | 100,000 per bucket                          |
| Write to disk, with `loseAtMost`      | every interval, 10,000 keys per transaction |

## See also

- [Rate limits](rate-limits.md): limit requests per second instead of
  counting them.
- [Quotas](quotas.md): limits per plan, over several windows at once.
- [kv/README.md](../../kv/README.md): the full contract of counters.
