# Branches

A branch is the part of a KV bucket that belongs to one owner: one user's
sessions, one team's flags. `under(owner)` gives the bucket seen from the
branch. A branch can be listed a page at a time and cleared in one call,
however many keys it holds.

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

## See also

- [KV](README.md): the engine's overview.
- [Sessions and limits](../../handbook/chat/sessions-and-limits.md#sessions):
  "sign out everywhere" in one call.
