# Versions

Every write of a KV key gives it a new version. Read a key with its version,
then write only if nobody changed it since: two editors of the same settings
page can no longer overwrite each other without knowing. `update` does the
read, the write and the retry in one call.

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

## Change a value without losing another write

Two requests that change the same key at once, such as a phone and a laptop
of the same user, each read the value, change it and write it back. With
`get` and `set`, the second write drops the first one's change without an
error. `update` keeps both:

```ts
// defaults: the settings a new key starts from
const next = await settings.update('home', current => ({ ...(current ?? defaults), theme: 'dark' }))
```

```python
# defaults: the settings a new key starts from
next = await settings.update("home", lambda current: replace(current or defaults, theme="dark"))
```

```go
// defaults: the settings a new key starts from
next, _, err := settings.Update(ctx, "home", func(current Settings, found bool) (Settings, bool) {
	if !found {
		current = defaults
	}
	current.Theme = "dark"
	return current, true
})
```

```rust
let next = settings.update("home", |current| {
    Some(Settings { theme: "dark".into(), ..current.unwrap_or_default() })
})?;
```

`update` reads the key and calls your function with its value, or with
nothing when the key is not there. It writes what your function returns at
the version it read, or creates the key. If another write came first,
`update` reads the key again and calls your function again, so no change is
lost. It returns the value that it wrote (contract KV-14).

> [!WARNING]
> **Your function can run more than once**
> Compute the new value from the one your function gets, and do nothing else
> in it. Send an email or call another service after `update` returns.

A key that is there keeps its expiry. A key that `update` creates gets the
bucket's `ttl` or `idle`, the same as with `set` and `create`.

## Leave the value as it is

Return nothing to write nothing: `undefined` in TypeScript, `None` in Python
and Rust, and `false` as the second result in Go. In TypeScript and Python,
returning the very object that your function got also writes nothing.
`update` compares with `===` and `is`, so an equal copy is written. When
nothing is written, `update` returns the value that is there.

```ts
const courses = store.bucket<number[]>('pinned-courses')

// pins course 2518 unless it is pinned already
await courses.update(studentId, pinned => (pinned?.includes(2518) ? pinned : [...(pinned ?? []), 2518]))
```

```python
courses = store.bucket("pinned-courses", list[int])

# pins course 2518 unless it is pinned already
await courses.update(student_id, lambda pinned: pinned if pinned and 2518 in pinned else [*(pinned or []), 2518])
```

```go
courses, err := kv.OpenBucket[[]int](store, "pinned-courses")

// pins course 2518 unless it is pinned already
pinned, _, err := courses.Update(ctx, studentID, func(pinned []int, found bool) ([]int, bool) {
	if slices.Contains(pinned, 2518) {
		return pinned, false
	}
	return append(pinned, 2518), true
})
```

```rust
let courses = store.bucket::<Vec<i64>>("pinned-courses").open()?;

// pins course 2518 unless it is pinned already
let pinned = courses.update(student_id, |pinned| {
    let mut pinned = pinned.unwrap_or_default();
    if pinned.contains(&2518) {
        return None;
    }
    pinned.push(2518);
    Some(pinned)
})?;
```

## When other writes keep coming first

`update` tries 8 times. If another write came first every time, it fails with
a conflict error (`ConflictError` in Bun and Python, `tinystore.ErrConflict` in
Go, `ErrorKind::Conflict` in Rust) and writes nothing:

```text
kv bucket settings: key "home": other writes came first each time it was tried
```

This happens only when many callers write the same key all the time. If the
value only adds up, use [counters](counters.md) instead. Otherwise, split the
key, for example by user.

## Where update works

- On a bucket of the store, on a branch (`under`), and on a bucket of a
  database (`db.bucket`).
- Inside a database's transaction in Bun, Python and Go, through
  `tx.with(bucket)` (`In(tx)` in Go). The transaction holds the database's
  writer, so no other write can come first, and your function runs once. In
  Rust, a bucket inside a transaction has no `update`: read with `get` and
  write with `set` there.
- Not inside a [transaction of the store](transactions.md). That transaction
  already runs your code again when a key that it read changed, so read with
  `get` and write with `set` there. A bucket in a store's transaction has no
  `update` in TypeScript, Python and Rust, and in Go it fails with an invalid
  error (`tinystore.ErrInvalid`).

## Async Rust

With the `async` feature, `update` and a key's call in `aio` take the same
arguments, and you await them. In Bun, Node and Python every call is already
awaited, and in Go a call blocks its goroutine, so this section shows only
Rust.

```rust
use tinystore::Options;
use tinystore::aio::Store;

let store = Store::open("data", Options::default())?;
let settings = store.bucket::<Settings>("settings").open()?;

let next = settings
    .update("home", |current| Some(Settings { theme: "dark".into(), ..current.unwrap_or_default() }))
    .await?;

let entry = settings.entry("home").await?.expect("home");
settings.key("home").if_version(entry.version).set(&next).await?; // a Conflict error if it changed
```

Your function runs on the thread that polls the future, so it must be `Send`
and `'static`. Each read and each write of `update` runs as a `get` and a
`set` do. If you drop the future after a write was queued, that write
commits, and no new try starts.

`key`, `if_version`, `ttl` and `expires_at` are plain calls, and only the
last step, `set`, `create`, `take`, `delete` or `expire`, is awaited. A ttl
or a time on a `take` or a `delete` fails with an invalid error
(`ErrorKind::Invalid`) before anything is written, as in blocking Rust.

## Limits and defaults

| What                                              | Value |
|---------------------------------------------------|-------|
| Tries before `update` fails with a conflict error | 8     |

## See also

- [KV](README.md): the engine's overview.
- [Transactions](transactions.md): reads that decide what to write, across
  keys.
