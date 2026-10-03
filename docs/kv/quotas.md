# Quotas

A quota limits how many times each key can do something within several time
windows at once. For example, each user can send 100 messages every 5 hours
and 300 messages per week. Quotas are part of the KV engine and are designed
for the limits of paid plans.

## Count a use

Open the quota once at startup and reuse it:

```ts
const ai = store.kv.quota('ai', { session: '100/5h', weekly: '300/7d' })
```

```python
ai = store.kv.quota("ai", session="100/5h", weekly="300/7d")
```

```go
state, err := kv.Open(ctx, store, kv.Options{})
ai, err := kv.OpenQuota(ctx, state, "ai",
	kv.Window("session", 100, 5*time.Hour),
	kv.Window("weekly", 300, 7*24*time.Hour))
```

Call `allow` before each action. It checks every window for the key. If all
of them have room, it counts the use in each window and saves the result to
disk:

```ts
const usage = await ai.allow(user.id)
if (!usage.ok) {
	const seconds = Math.ceil(usage.retryAfter / 1000)
	return new Response('Limit reached', { status: 429, headers: { 'Retry-After': `${seconds}` } })
}
```

```python
import math
from fastapi import HTTPException

usage = await ai.allow(user.id)
if not usage.ok:
    seconds = math.ceil(usage.retry_after)
    raise HTTPException(429, "Limit reached", headers={"Retry-After": str(seconds)})
```

```go
usage, err := ai.Allow(r.Context(), user.ID)
switch {
case err != nil:
	http.Error(w, "Unavailable", http.StatusServiceUnavailable)
	return
case !usage.OK:
	seconds := int(math.Ceil(usage.RetryAfter.Seconds()))
	w.Header().Set("Retry-After", strconv.Itoa(seconds))
	http.Error(w, "Limit reached", http.StatusTooManyRequests)
	return
}
```

If any window is full, `allow` returns `ok: false` and counts nothing. This is
a normal result, not an error. The retry time tells you when the full window
resets:

| | Field | Unit |
|---|---|---|
| Bun | `retryAfter` | milliseconds |
| Python | `retry_after` | seconds |
| Go | `RetryAfter` | `time.Duration` |

## All windows or none

If you checked two limits one after the other, the first one could count a use
even though the second one rejects it. A quota checks all of its windows in a
single write, so this can't happen.

For example, a user has sent 12 of 100 messages in the current 5-hour window
and 300 of 300 this week. The next `allow` is rejected because the weekly
window is full, and the 5-hour window stays at 12.

Concurrent requests are safe too. If two requests arrive for the last
remaining use, only one of them is allowed.

## Each key has its own windows

A window starts the first time a key is used, and lasts for its full length.
The next window starts with the next use after that. Windows don't reset at
midnight or on Monday for everyone at once. Each key resets on its own
schedule:

```text
session, 5h   first use at 13:42 → resets at 18:42; next use at 19:07 → resets at 00:07
weekly, 7d    first use on Tuesday at 13:42 → resets next Tuesday at 13:42
```

When all windows of a key have reset, TinyStore deletes the key's data
automatically. You don't need a cleanup job.

## Show remaining usage

Use `get` to read a key's windows without counting a use, for example to draw
usage bars on a settings page. Its `ok` field tells you whether one more use
would be allowed right now.

```ts
const { windows } = await ai.get(user.id)
windows.weekly.used    // 37
windows.weekly.left    // 263
windows.weekly.resetAt // a Date, or undefined if the window hasn't started
```

```python
usage = await ai.get(user.id)
weekly = usage.windows["weekly"]
weekly.used      # 37
weekly.left      # 263
weekly.reset_at  # a datetime, or None if the window hasn't started
```

```go
usage, err := ai.Get(ctx, user.ID)
weekly := usage.Windows["weekly"]
fmt.Println(weekly.Used, weekly.Left) // 37 263
fmt.Println(weekly.ResetAt)           // the zero time if the window hasn't started
```

In TypeScript, the window names are part of the quota's type. `windows.weekly`
compiles, but a typo like `windows.wekly` doesn't.

## Count several uses at once

To count more than one use in a single call, for example the tokens of an AI
request, pass a count: `allow(key, n)` in Bun and Python, or
`AllowN(ctx, key, n)` in Go. Either all `n` uses are counted, or none are.

If `n` is larger than the smallest window's limit, the call could never
succeed. In that case `allow` fails with an invalid argument error:
`ErrInvalid` in Go, `InvalidError` in Bun and Python.

## Refund uses

If an action fails on your side after `allow` counted it, for example when a
model call times out, give the use back with `refund(key)` or `refund(key, n)`.
In Go, use `Refund` or `RefundN`. A refund only goes to windows that haven't
reset since the use, and a count never drops below zero.

To reset a key completely, for example when support lifts a user's limit,
call `delete(key)`. The key's next use starts all of its windows from scratch.

## Separate counts per workspace

Use `of` to keep separate counts for the same key in different contexts. For
example, a user who belongs to two workspaces gets a separate quota in each
one:

```ts
const usage = await ai.of(workspace.id).allow(user.id)
```

```python
usage = await ai.of(workspace.id).allow(user.id)
```

```go
usage, err := ai.Of(workspace.ID).Allow(ctx, user.ID)
```

## Change the limits

The windows are defined in your code, not in the stored data, so you can change
them between deploys:

- If you change a window's limit, the uses already counted in it still count.
- If you add a window, it starts for each key at that key's next use.
- If you remove a window, its counts are discarded.

## Crash safety

`allow` writes every allowed use to disk before it returns, so a crash or a
restart never loses a count. This is why quotas fit plan limits.

To limit a request rate, use the KV engine's
[rate limiter](../../kv/README.md#limiter) instead. It keeps its state in
memory, so each check is cheaper, but a crash can let one extra burst of
requests through.

## Limits and defaults

| | |
|---|---|
| Windows per quota | 1 to 8 |
| Window name | `[a-z][a-z0-9_]{0,31}` |
| Window | a count and a length: `'100/5h'` or `'300/7d'` in Bun and Python, `kv.Window("weekly", 300, 7*24*time.Hour)` in Go |
| Uses per call | 1 up to the smallest window's limit |
| Key | a string or an integer, up to 1 KiB including its branch |

An invalid window, or more than 8 windows, fails immediately. If the quota's
name is already used by another kind of KV bucket, opening the quota fails. In
Go this happens in `OpenQuota`, and in Bun and Python on the first call.

## See also

- [kv/README.md](../../kv/README.md#quota): the full contract of quotas.
- [design/kv.md](https://github.com/tinyshed/research/blob/main/tinystore/design/kv.md#quota)
  in the research repository: why quotas work this way.
