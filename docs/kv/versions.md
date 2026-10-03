# Versions

Every KV write gives the key a new version. Read the version with the value,
and write back only if nobody changed the key in between. Use versions for
optimistic locking, such as two browser tabs editing one draft, and for claims,
such as a webhook that arrives twice.

## Save only if nobody else saved

```ts
const entry = await drafts.of(userId).getEntry(noteId)
// … the user edits the draft in the browser …

try {
	await drafts.of(userId).set(noteId, edited, { ifVersion: entry.version })
} catch (err) {
	if (err instanceof ConflictError) {
		// another tab saved first: show both versions to the user
	}
}
```

```python
entry = await drafts.of(user_id).get_entry(note_id)
# … the user edits the draft in the browser …

try:
    await drafts.of(user_id).set(note_id, edited, if_version=entry.version)
except ConflictError:
    ...  # another tab saved first: show both versions to the user
```

```go
entry, found, err := drafts.Of(userID).GetEntry(ctx, noteID)
// … the user edits the draft in the browser …

err = drafts.Of(userID).Set(ctx, noteID, edited, kv.IfVersion(entry.Version))
if errors.Is(err, tinystore.ErrConflict) {
	// another tab saved first: show both versions to the user
}
```

`getEntry` returns the value with its version and expiry. A write with
`ifVersion` succeeds only if the key still has that version. If the key
changed, was deleted or expired, the write fails with a conflict error and
changes nothing.

A version is an opaque string. You can put it in a hidden form field, send it
to the browser, and pass it back with the next save. Compare versions only for
equality.

## Versions never repeat

A key's version changes with every write, and a version is never used twice,
even after the key is deleted, expires or the store restarts. An old version
can't match a key that was deleted and created again.

Changing only the expiry, with `touch` or a [sliding expiry](expiry.md), keeps
the version. Extending a session never breaks another writer's `ifVersion`.

## Handle a webhook exactly once

A payment provider sends an event until it gets a `200`, so the same event can
arrive twice, even at the same moment. Claim the event before you handle it:

```ts
const events = store.kv.bucket('stripe-events', 'none')

const { created, entry } = await events.setEntryIfAbsent(event.id, null, { ttl: '10m' })
if (!created) {
	return // handled before, or being handled right now
}
try {
	await handle(event)
} catch (err) {
	await events.delete(event.id, { ifVersion: entry.version }) // let the next delivery retry
	throw err
}
await events.set(event.id, null, { ifVersion: entry.version, ttl: '7d' })
```

```python
events = store.kv.bucket("stripe-events", None)

created, entry = await events.set_entry_if_absent(event.id, None, ttl="10m")
if not created:
    return  # handled before, or being handled right now
try:
    await handle(event)
except Exception:
    await events.delete(event.id, if_version=entry.version)  # let the next delivery retry
    raise
await events.set(event.id, None, if_version=entry.version, ttl="7d")
```

```go
events, err := kv.OpenBucket[struct{}](ctx, state, "stripe-events")

claim, created, err := events.SetEntryIfAbsent(ctx, event.ID, struct{}{}, kv.TTL(10*time.Minute))
if err != nil || !created {
	return err // handled before, or being handled right now
}
if err := handle(ctx, event); err != nil {
	return errors.Join(err, events.Delete(ctx, event.ID, kv.IfVersion(claim.Version)))
}
return events.Set(ctx, event.ID, struct{}{}, kv.IfVersion(claim.Version), kv.TTL(7*24*time.Hour))
```

`setEntryIfAbsent` writes only if the key doesn't exist, in one step. If two
deliveries arrive together, one of them creates the claim and the other one
sees `created` as false.

The claim expires after ten minutes, so an event whose handler crashed is
handled again on the next delivery. The version stops a slow handler whose
claim already expired from finishing or deleting a newer claim.

> [!NOTE]
> **The limit of two systems**
> If your handler calls another service and then crashes before it marks the
> event as done, the next delivery handles it again. Pass the event id to the
> other service as its idempotency key too. [Once](once.md) does this for you
> in a single call.

## Calls that take a version

| Call | With a version |
|---|---|
| `set`, `setEntry` | writes only if the key still has the version |
| `delete` | deletes only that version of the key |
| `take` | reads and deletes only that version |
| `touch` | changes the expiry only for that version |

In Python, the option is `if_version`. In Go, it is `kv.IfVersion(v)`.

## See also

- [Transactions](transactions.md): several writes that succeed or fail
  together.
- [kv/README.md](../../kv/README.md): the full contract of versions.
