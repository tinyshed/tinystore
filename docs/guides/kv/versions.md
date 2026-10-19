# Versions

Every write of a KV key gives it a new version. Read a key with its version,
then write only if nobody changed it since: two editors of the same settings
page can no longer overwrite each other without knowing.

```ts
const settings = store.bucket<Settings>('settings')

const entry = await settings.entry('home') // { key, value, version, expiresAt } | undefined
await settings.set('home', next, { ifVersion: entry!.version }) // ConflictError if it changed
```

```python
settings = store.bucket("settings", Settings)

entry = await settings.entry("home")  # Entry(key, value, version, expires_at) | None
await settings.set("home", next, if_version=entry.version)  # ConflictError if it changed
```

```go
settings, err := kv.OpenBucket[Settings](store, "settings")

entry, found, err := settings.Entry(ctx, "home") // kv.Entry[Settings]{Key, Value, Version, ExpiresAt}
err = settings.Set(ctx, "home", next, kv.IfVersion(entry.Version)) // tinystore.ErrConflict if it changed
```

```rust
let settings = store.bucket::<Settings>("settings").open()?;

let entry = settings.entry("home")?; // Option<Entry { key, value, version, expires_at }>
settings.key("home").if_version(entry.unwrap().version).set(&next)?; // a Conflict error if it changed
```

## A safe read-modify-write

```ts
for (;;) {
	const entry = await settings.entry('home')
	const next = { ...entry!.value, theme: 'dark' }
	try {
		await settings.set('home', next, { ifVersion: entry!.version })
		break
	} catch (err) {
		if (!(err instanceof ConflictError)) throw err // someone else wrote: read again
	}
}
```

```python
while True:
    entry = await settings.entry("home")
    try:
        await settings.set("home", replace(entry.value, theme="dark"), if_version=entry.version)
        break
    except tinystore.ConflictError:
        continue  # someone else wrote: read again
```

```go
for {
	entry, _, err := settings.Entry(ctx, "home")
	if err != nil {
		return err
	}
	next := entry.Value
	next.Theme = "dark"
	err = settings.Set(ctx, "home", next, kv.IfVersion(entry.Version))
	if !errors.Is(err, tinystore.ErrConflict) {
		return err // nil when it was written
	}
}
```

```rust
loop {
    let entry = settings.entry("home")?.expect("home");
    let next = Settings { theme: "dark".into(), ..entry.value };
    match settings.key("home").if_version(entry.version).set(&next) {
        Err(err) if err.kind() == ErrorKind::Conflict => continue, // someone else wrote: read again
        other => break other?,
    }
}
```

## What a version is

- A version is the revision of `kv.db` that wrote the value. It never repeats,
  across deletes, expiry and restarts, and it only compares for equality.
- `ifVersion` works on `set`, `take`, `delete` and `expire`. A key that
  changed, expired or was deleted since is a `conflict`, and nothing is
  written.
- In TypeScript `ifVersion` takes a version and never `undefined`, so
  `{ ifVersion: entry?.version }` does not compile: a key that is not there
  would be written without a check. Python refuses `if_version=None` for the
  same reason.

Two writes that only depend on themselves need no version: `create` writes
only when the key is not there, and `take` reads and deletes in one write.
For reads that decide several writes, use a [transaction](transactions.md).

## See also

- [KV](README.md): the engine's overview.
- [Transactions](transactions.md): reads that decide what to write, across
  keys.
