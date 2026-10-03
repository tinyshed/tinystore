# Expiry

KV keys can expire. A key can live for a fixed time after it is created, until
a time you set, or for as long as it keeps being read. An expired key behaves
exactly like a deleted one, and TinyStore removes it from disk in the
background, so you never write a cleanup job.

## Expire every key of a bucket

```ts
const codes = store.kv.bucket('sign-in-codes', 'int', { defaultTtl: '15m' })

await codes.set('K7Q2', 42) // expires 15 minutes from now
```

```python
codes = store.kv.bucket("sign-in-codes", int, default_ttl="15m")

await codes.set("K7Q2", 42)  # expires 15 minutes from now
```

```go
codes, err := kv.OpenBucket[int64](ctx, state, "sign-in-codes", kv.DefaultTTL(15*time.Minute))

err = codes.Set(ctx, "K7Q2", 42) // expires 15 minutes from now
```

The default TTL (`DefaultTTL`, `defaultTtl`, `default_ttl`) applies when a
key is created. A later `set` of the same key keeps the expiry the key
already has, so writing a key again doesn't extend its life.

## Expire one key

```ts
await cache.set('rates', rates, { ttl: '1h' })
await cache.set('report', report, { expireAt: endOfDay })
```

```python
await cache.set("rates", rates, ttl="1h")
await cache.set("report", report, expire_at=end_of_day)
```

```go
err = cache.Set(ctx, "rates", rates, kv.TTL(time.Hour))
err = cache.Set(ctx, "report", report, kv.ExpireAt(endOfDay))
```

A TTL or an expiry time on a write replaces the key's current expiry.

## Keep a key alive while it is used

```ts
const sessions = store.kv.bucket<Session>('sessions', { sliding: '30d' })

await sessions.get(token) // the session now lives 30 more days
```

```python
sessions = store.kv.bucket("sessions", Session, sliding="30d")

await sessions.get(token)  # the session now lives 30 more days
```

```go
sessions, err := kv.OpenBucket[Session](ctx, state, "sessions", kv.Sliding(30*24*time.Hour))

s, found, err := sessions.Get(ctx, token) // the session now lives 30 more days
```

With a sliding expiry, a key lives for the given time after it was last read
with `get`, `getEntry` or `has`. A user who comes back every week stays signed
in, and one who doesn't return is signed out after 30 days.

A read doesn't write to disk. TinyStore extends a key at most once per 1/30 of
its time, once a day for 30 days, and saves the new expiry in the background
within a second. A key that is read in its last minute is extended before the
read returns, so it can't expire in between. `scan` and `all` don't extend
keys.

A bucket can't have both a default TTL and a sliding expiry.

## Change an expiry without changing the value

```ts
await sessions.touch(token, { ttl: '1h' }) // true if the key exists
```

```python
await sessions.touch(token, ttl="1h")  # True if the key exists
```

```go
found, err := sessions.Touch(ctx, token, kv.TTL(time.Hour)) // true if the key exists
```

`touch` sets a new expiry and keeps the value and its version, so it never
causes a version conflict for another writer.

## What expired means

Once a key expires, every call treats it as deleted:

- `get`, `has`, `scan` and `all` don't see it;
- `take` finds nothing;
- `setIfAbsent` can create the key again;
- a write with a version of the old key fails with a conflict;
- a [counter](counters.md) starts again from zero.

TinyStore deletes expired keys in the background, up to 600,000 keys per
minute, so a bucket that creates many short-lived keys doesn't grow forever.

## Test expiry without waiting

In Go, give the store a clock that your test controls, and nothing has to
sleep:

```go
now := time.Now()
store, err := tinystore.Open(ctx, t.TempDir(), tinystore.Options{
	Manual: true,
	Clock:  func() time.Time { return now },
})
state, err := kv.Open(ctx, store, kv.Options{})
codes, err := kv.OpenBucket[int64](ctx, state, "sign-in-codes", kv.DefaultTTL(15*time.Minute))

err = codes.Set(ctx, "K7Q2", 42)
now = now.Add(16 * time.Minute)
_, found, err := codes.Take(ctx, "K7Q2") // false: the code expired
```

`Manual` turns off background work, so the test decides when maintenance
runs with `state.Maintain(ctx)`.

## Limits and defaults

| | |
|---|---|
| Default TTL | none: keys don't expire unless you set one |
| Sliding extension | at most once per 1/30 of the sliding time |
| Expired keys deleted | up to 600,000 per minute |

## See also

- [kv/README.md](../../kv/README.md): the full contract of expiry.
- [Sessions](sessions.md): sliding expiry in a complete sign-in flow.
