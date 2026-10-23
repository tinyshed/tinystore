# Expiry

A key in a KV bucket can expire: a fixed time after it was written, at a set
time, or after a span without reads or writes. An expired key is gone for
every call at once, and nothing has to clean it up.

```ts
const codes = store.bucket<number>('sign-in-codes', { ttl: '15m' })
const sessions = store.bucket<Session>('sessions', { idle: '30d' })

await codes.set(digest(code), userId) // expires 15 minutes from now
await codes.set(digest(code), userId, { ttl: '1h' }) // this key's own term
await codes.set(digest(code), userId, { expiresAt: endOfDay }) // at a time
await codes.expire(digest(code), '5m') // a new term for a key that exists
```

```python
codes = store.bucket("sign-in-codes", int, ttl="15m")
sessions = store.bucket("sessions", Session, idle="30d")

await codes.set(digest(code), user_id)  # expires 15 minutes from now
await codes.set(digest(code), user_id, ttl="1h")  # this key's own term
await codes.set(digest(code), user_id, expires_at=end_of_day)  # at a time
await codes.expire(digest(code), "5m")  # a new term for a key that exists
```

```go
codes, err := kv.OpenBucket[int64](store, "sign-in-codes", kv.TTL(15*time.Minute))
sessions, err := kv.OpenBucket[Session](store, "sessions", kv.Idle(30*24*time.Hour))

err = codes.Set(ctx, digest(code), userID)                   // expires 15 minutes from now
err = codes.Set(ctx, digest(code), userID, kv.TTL(time.Hour)) // this key's own term
err = codes.Expire(ctx, digest(code), 5*time.Minute)          // a new term for a key that exists
```

```rust
let codes = store.bucket::<i64>("sign-in-codes").ttl(Duration::from_mins(15)).open()?;
let sessions = store.bucket::<Session>("sessions").idle(Duration::from_hours(24 * 30)).open()?;

codes.set(digest(&code), &user_id)?; // expires 15 minutes from now
codes.key(digest(&code)).ttl(Duration::from_hours(1)).set(&user_id)?; // this key's own term
codes.expire(digest(&code), Duration::from_mins(5))?; // a new term for a key that exists
```

## The three kinds of term

| Option            | The key expires                                         | For                              |
|-------------------|---------------------------------------------------------|----------------------------------|
| `ttl: '15m'`      | 15 minutes after it was written                         | codes, caches, links that expire |
| `expiresAt: date` | at that time                                            | a trial that ends on a date      |
| `idle: '30d'`     | 30 days after it was last read or written               | sessions of active users         |
| none              | never, unless the bucket's default or the call sets one |                                  |

The bucket's `ttl` or `idle` is the default for every key written without its
own. A bucket has one or the other, never both. `expiresAt` is
`expires_at` in Python and `kv.ExpiresAt` in Go, and `expire` takes a time
as `ExpireAt` in Go.

## What "expired" means

An expired key is absent to every call at once (contract KV-03): `get`
returns nothing, `create` and `add` can write it again, and a write with
`ifVersion` fails `conflict`. The store deletes the row later, in the
background, so expiry costs no write at the moment it happens.

## Idle keys

A `get`, `entry` or `has` of an idle key renews its span. To keep reads
cheap, the renewal is written only once a thirtieth of the span has passed,
about once a day for 30 days, together with other writes a second later.
`list` and `all` renew nothing, so a page that lists every session does not
keep them alive. A read on a copy of the store's files sends its renewal to
the copy's home, so a session read only on a copy stays alive too.
[Copies](../../handbook/nodes/copies.md#reads-are-local) says what that
costs.

## Limits and defaults

| What                  | Default                                 |
|-----------------------|-----------------------------------------|
| a bucket's term       | none: keys never expire                 |
| an idle key's renewal | once a thirtieth of its span has passed |
| when a row is deleted | in the background, after it expired     |

## See also

- [KV](README.md): the engine's overview.
- [Sessions and limits](../../handbook/chat/sessions-and-limits.md): codes
  that expire and sessions that end when idle.
- [Test on a clock you move](../../handbook/chat/keeping-it.md#test-on-a-clock-you-move):
  test expiry without waiting.
