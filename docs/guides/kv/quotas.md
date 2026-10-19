# Quotas

A quota counts uses against a plan, in named windows that reset on their
own: 100 messages per 5 hours and 300 per week. `allow` counts a use in every
window or in none, answers when each window resets, and `refund` gives a use
back when the work failed.

```ts
const messages = store.quota('messages', { session: '100/5h', weekly: '300/7d' })

const used = await messages.allow(userId)
if (!used.ok) return { status: 429, retryAt: used.retryAt, weekly: used.windows.weekly }
await messages.refund(userId) // the model failed: give the use back
```

```python
messages = store.quota("messages", session="100/5h", weekly="300/7d")

used = await messages.allow(user_id)
if not used.ok:
    return Response(429, retry_at=used.retry_at, weekly=used.windows["weekly"])
await messages.refund(user_id)  # the model failed: give the use back
```

```go
messages, err := kv.OpenQuota(store, "messages",
	kv.Window("session", 100, 5*time.Hour), kv.Window("weekly", 300, 7*24*time.Hour))

used, err := messages.Allow(ctx, strconv.FormatInt(userID, 10))
if err == nil && !used.OK {
	return tooMany(used.RetryAt, used.Windows["weekly"])
}
err = messages.Refund(ctx, strconv.FormatInt(userID, 10)) // the model failed: give the use back
```

```rust
let messages = store
    .quota("messages")
    .window("session", 100, Duration::from_hours(5))
    .window("weekly", 300, Duration::from_hours(24 * 7))
    .open()?;

let used = messages.allow(user_id)?;
if !used.ok {
    return Ok(too_many(used.retry_at, used.window("weekly")));
}
messages.refund(user_id)?; // the model failed: give the use back
```

## What an answer holds

```text
{ ok: false, left: 0, retryAt: 2026-10-20T09:20:00Z,
  windows: { session: { used: 100, limit: 100, left: 0, resetsAt: … },
             weekly:  { used: 212, limit: 300, left: 88, resetsAt: … } } }
```

Each window's `used`, `limit`, `left` and `resetsAt` (`resets_at`,
`ResetsAt`) let a page say "88 messages left this week, resets on Monday".

## How it behaves

- A window starts at the key's first use and resets on its own, a span
  later.
- `allow` counts in every window, or in none: a use refused because the
  weekly window is full does not use up the 5-hour window.
- `allow(key, n)` counts `n` uses at once; more than a window's limit at once
  never passes and is `invalid`.
- `peek(key)` gives the answer without counting; `reset(key)` forgets the key.
- A quota is written on every `allow`, unlike a rate limit: a plan's limit is
  a promise to a customer, and a crash must not hand out free uses.

| What              | Limit                     |
|-------------------|---------------------------|
| windows per quota | one to eight              |
| a window's rate   | as `'100/5h'`, `'300/7d'` |

## See also

- [KV](README.md): the engine's overview.
- [Rate limits](rate-limits.md): smoothing traffic instead of counting a plan.
- [Limits per plan](../../handbook/chat/sessions-and-limits.md#limits-per-plan)
  in the Handbook.
