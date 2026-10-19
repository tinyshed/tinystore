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
- An expired or cleared key is absent to every call at once, with no cleanup
  job (contract KV-03).
- A version changes on every write and a write with `ifVersion` fails
  `conflict` if the key changed since it was read.
- `take`, `create` and `add` decide in one write: of two callers, one wins.
- `once` runs a key's function once while its answer is kept, and callers in
  any process wait for it (contract KV-11).

## In these guides

| Page              | Covers                                                   |
|-------------------|----------------------------------------------------------|
| Expiry            | `ttl`, `idle`, `expiresAt`, `expire`                     |
| Versions          | `entry`, `ifVersion`, safe read-modify-write             |
| Branches          | `under`: one user's keys, listed and cleared in one call |
| Encrypted buckets | values sealed with the store's key                       |
| Counters          | totals by key, in memory or written every add            |
| Rate limits       | a smooth rate with a burst, `allow`, `peek`, `reset`     |
| Quotas            | uses in windows that reset, `refund`                     |
| Once              | requests sent twice, and memos                           |
| Transactions      | `store.tx`: reads that decide what to write              |

New to KV? [Sessions and limits](../../handbook/chat/sessions-and-limits.md)
in the Handbook signs users in with codes, sessions, a rate limit, a quota and
`once`.

## Reference

[Bun and Node](../../reference/bun/kv.md) ·
[Python](../../reference/python/kv.md) ·
[Go](../../reference/go/kv.md) ·
[Rust](../../reference/rust/kv.md)
