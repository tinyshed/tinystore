# Repair

A disk can change a byte. When a read meets a segment or a waiting row whose
bytes no longer read, it fails `corrupt` and names what broke, rather than
answering without it. `damaged` lists what the store has met that no longer
reads, and `drop` removes it, so the reads over it work again. Packing and
merging go past a damaged segment meanwhile (contract ERR-07).

```ts
for (const damage of await store.events().damaged()) {
	console.log(damage.part, damage.stream, damage.first, damage.last, damage.error) // what broke, and when
	await store.events().drop(damage)
}
```

```python
for damage in await store.events().damaged():
    print(damage.part, damage.stream, damage.first, damage.last, damage.error)
    await store.events().drop(damage)
```

```go
damaged, err := events.Damaged(ctx)
for _, damage := range damaged {
	err = events.Drop(ctx, damage)
}
```

```rust
let events = store.all_events()?;
for damage in events.damaged()? {
    events.drop(&damage)?;
}
```

## What happens, step by step

1. A read, a pack or a merge meets bytes whose checksum does not match. The
   read fails `corrupt`, naming the stream and the segment; the store keeps a
   note of it.
2. Every later read over the same range fails the same way. A read of other
   streams, or of another time of the same stream, is not affected.
3. Packing and merging skip the damaged segment and go on with the rest, so
   new events keep being written and packed.
4. `damaged` lists every damage the store has met: a head row, which held
   events not packed yet, or a segment; its stream, its events' oldest and
   newest times, and what broke.
5. `drop` removes it. The events it held are gone; every read over that
   range works again. `drop` refuses what still reads.

A store does not drop damage by itself: deciding to lose a stream's hour is
the program's, or a person's.

## Before dropping

What a damaged segment held may still be in a backup. Restore the backup into
another directory, read the range from it, and append what matters into the
live store before `drop`:

```ts
const old = await open('./restored', { private: true })
const lost = await old.events('audit').list({ from: damage.first, to: new Date(damage.last.getTime() + 1), limit: 10_000 })
await audit.append(lost.events.map(e => ({ name: e.name, fields: e.fields, at: e.at })))
```

An event older than its stream keeps cannot be appended again: what `keep`
already passed is gone in any case.

## See also

- [Records](README.md): the engine's overview.
- [Backup](../backup/README.md): a backup checks every byte when it is
  restored.
