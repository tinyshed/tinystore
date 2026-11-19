# Branches

A branch is the part of a KV bucket that belongs to one owner: one user's
sessions, one team's flags. `under(owner)` gives the bucket seen from the
branch. A branch can be listed a page at a time, counted, and cleared in one
call, however many keys it holds.

```ts
const sessions = store.bucket<Session>('sessions', { idle: '30d' })

await sessions.under(userId).set(digest(token), { userId, device: 'web' })
const page = await sessions.under(userId).list({ limit: 100 }) // { entries, next }
for await (const { key, value } of sessions.under(userId).all()) show(key, value)
await sessions.under(userId).clear() // signed out everywhere
```

```python
sessions = store.bucket("sessions", Session, idle="30d")

await sessions.under(user_id).set(digest(token), Session(user_id=user_id, device="web"))
page = await sessions.under(user_id).list(limit=100)  # Page(entries, next)
async for entry in sessions.under(user_id).all():
    show(entry.key, entry.value)
await sessions.under(user_id).clear()  # signed out everywhere
```

```go
sessions, err := kv.OpenBucket[Session](store, "sessions", kv.Idle(30*24*time.Hour))

err = sessions.Under(userID).Set(ctx, digest(token), Session{UserID: userID, Device: "web"})
page, err := sessions.Under(userID).List(ctx, kv.Limit(100)) // kv.Page[Session]{Entries, Next}
for entry, err := range sessions.Under(userID).All(ctx) {
	if err != nil {
		return err
	}
	show(entry.Key, entry.Value)
}
err = sessions.Under(userID).Clear(ctx) // signed out everywhere
```

```rust
let sessions = store.bucket::<Session>("sessions").idle(Duration::from_hours(24 * 30)).open()?;

sessions.under(user_id).set(digest(&token), &session)?;
sessions.under(user_id).clear()?; // signed out everywhere
```

A page holds entries, not bare values: each value comes with its key, its
version and when it expires. Two keys of a bucket of targets, one set with a
`ttl`, list as one page with no `next`:

```json
{
  "entries": [
    { "key": "api", "value": { "url": "https://api.example.com/health" }, "version": "2", "expiresAt": "2026-10-21T21:23:48.122Z" },
    { "key": "home", "value": { "url": "https://example.com" }, "version": "1" }
  ]
}
```

## How branches work

- A branch exists while it holds keys; there is nothing to create or delete.
- Owners are kept apart from keys and from each other: `under('a').under('b')`
  is never `under('a/b')`, and no key can climb into another branch.
- `clear()` removes a branch and every branch under it, at once for every
  reader. On the bucket itself, it empties the bucket.
- `list()` reads one page, at most 1,000 keys, in the byte order of the keys
  (`"10"` before `"9"`). Pass the page's `next` as `after` for the page after
  it. A full page has a `next` even when no key follows, and the page after
  it is then empty; a page with no `next` is the last. `all()` walks every
  page, holding nothing between pages.
- On the bucket itself, without `under`, `list()` and `all()` read the keys
  of its root: a bucket of targets or settings is listed the same way.
- Every call works on a branch: `get`, `set`, `take`, `create`, `expire`,
  `delete`, `entry`.

## How many keys

`count()` says how many keys `all()` would walk now: the branch's own keys,
not those of the branches under it. On the bucket itself it counts the root's
keys.

```ts
const left = await uploads.count() // 1834
const mine = await sessions.under(userId).count() // one user's sessions
```

```python
left = await uploads.count()  # 1834
mine = await sessions.under(user_id).count()  # one user's sessions
```

```go
left, err := uploads.Count(ctx) // int64
mine, err := sessions.Under(userID).Count(ctx)
```

```rust
let left = uploads.count()?; // u64
let mine = sessions.under(user_id).count()?;
```

- An expired key is not counted, even before it is deleted from the file, and
  neither is a cleared one (contract KV-15). Counting an `idle` key does not
  keep it alive.
- It reads every key of the branch, so its cost grows with the branch. A large
  branch is counted in several reads of 65,536 keys, so a key written during
  the count may or may not be counted.
- `count` is not a call of a transaction: `In(tx)` in Go refuses it
  `invalid`, and a transaction's bucket in the other languages has no `count`.
- `tinystore status ./data --names` counts every bucket whole, every branch
  included, beside the program that runs it.

## Async Rust

With the `async` feature, `aio`'s buckets read pages the same way, and you
await them. See [Rust with async](../../handbook/start/rust-async.md).

```rust
use futures::StreamExt; // or tokio_stream::StreamExt
use tinystore::Options;
use tinystore::aio::Store;

let store = Store::open("data", Options::default())?;
let sessions = store.bucket::<Session>("sessions").open()?;

let page = sessions.under(user_id).list(100, None).await?;
let keys = sessions.under(user_id).count().await?;
let mut all = sessions.under(user_id).all(); // a Stream of Result<Entry<Session>>
while let Some(entry) = all.next().await {
    let entry = entry?;
    println!("{} until {:?}", entry.key, entry.expires_at);
}
```

`all` reads the next page only when you have used up the one before. When the
store closes, the stream returns a closed error (`ErrorKind::Closed`) and then
ends.

## See also

- [KV](README.md): the engine's overview.
- [Sessions and limits](../../handbook/chat/sessions-and-limits.md#sessions):
  "sign out everywhere" in one call.
