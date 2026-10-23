# Copies and expiry

A copy costs a row, whatever the file's size: stored bytes never change, so
two files can share them. A rename is a copy and a delete in one write. A
file can expire a span after it was written, and its bytes go once nothing
names them.

```ts
await docs.copy('notes.md', 'notes (copy).md') // FileInfo; no bytes are copied
await docs.rename('draft.md', 'final.md') // ConflictError if final.md is there
```

```python
await docs.copy("notes.md", "notes (copy).md")
await docs.rename("draft.md", "final.md")
```

```go
_, err = docs.Copy(ctx, "notes.md", "notes (copy).md")
_, err = docs.Rename(ctx, "draft.md", "final.md")
```

```rust
docs.copy("notes.md", "notes (copy).md")?;
docs.rename("draft.md", "final.md")?;
```

## Copies never replace by surprise

`put` and an upload are writes of a path and replace what it held. `copy` and
`rename` move a file that exists onto a path that may hold one, so a path
that holds a file is `conflict`, unless `ifMatch` names the version to
replace:

```ts
const final = await docs.head('final.md')
await docs.rename('draft.md', 'final.md', { ifMatch: final!.etag }) // replaces exactly that version
```

```python
final = await docs.head("final.md")
await docs.rename("draft.md", "final.md", if_match=final.etag)
```

```go
final, found, err := docs.Head(ctx, "final.md")
_, err = docs.Rename(ctx, "draft.md", "final.md", blobs.IfMatch(final.ETag))
```

```rust
let final_ = docs.head("final.md")?.expect("final.md");
docs.key("final.md").if_match(&final_.etag).rename_from("draft.md")?;
```

| Call             | Source missing | Target holds a file          |
|------------------|----------------|------------------------------|
| `put`, `upload`  | —              | replaced                     |
| `copy`, `rename` | `not found`    | `conflict`, unless `ifMatch` |
| `create`         | —              | nothing written, `false`     |

A copy and its source are two files from then on: replacing or deleting one
leaves the other, and a copy outlives its source (contract BLOB-05). Both
stay within one set of files; between two sets, read and `put`.

## Expiry

```ts
const reports = store.files('reports', { ttl: '24h' })

await reports.put('report.csv', csv) // expires 24 hours from now
await reports.put('report.csv', csv, { ttl: '7d' }) // its own term
await reports.expire('report.csv', '1h') // from now; a Date for a time
```

```python
reports = store.files("reports", ttl="24h")

await reports.put("report.csv", csv)
await reports.put("report.csv", csv, ttl="7d")
await reports.expire("report.csv", "1h")  # a datetime for a time
```

```go
reports, err := blobs.OpenFiles(store, "reports", blobs.TTL(24*time.Hour))

_, err = reports.Put(ctx, "report.csv", csv)
_, err = reports.Put(ctx, "report.csv", csv, blobs.TTL(7*24*time.Hour))
err = reports.Expire(ctx, "report.csv", time.Hour) // ExpireAt for a time
```

```rust
let reports = store.files("reports").ttl(Duration::from_hours(24)).open()?;

reports.put("report.csv", &csv)?;
reports.key("report.csv").ttl(Duration::from_hours(24 * 7)).put(&csv)?;
reports.expire("report.csv", Duration::from_hours(1))?;
```

- The set's `ttl` is the term of every file written without its own, not a
  ceiling: a write that gives its own keeps it.
- A term counts from the write, and every write gets its own: an export
  written again lives its day again.
- An expired file is gone for readers at once: `get` and `head` give nothing.
  Its bytes are removed in the background, within a minute.
- `expires` on a file says when it goes. A term that ends after the year
  9999 reads as 9999-12-31 23:59:59 UTC, on every platform.

A backup does not belong in files with a `ttl`.

## Deleting

```ts
await docs.delete('notes.md') // nothing to delete is not an error
await docs.folder('users', 42).clear() // a folder, at once, however many files
```

```python
await docs.delete("notes.md")
await docs.folder("users", 42).clear()
```

```go
err = docs.Delete(ctx, "notes.md")
err = docs.Folder("users", 42).Clear(ctx)
```

```rust
docs.delete("notes.md")?;
docs.folder("users").folder(42).clear()?;
```

A delete, a clear, an expiry or a replace drops the file's name in one
commit. The bytes stay as long as a copy or a reader that opened them still
holds them, then go in the background.

## See also

- [Blobs](README.md): the engine's overview.
- [Integrity](integrity.md): `ifMatch` and `create` for writes that must not
  cross.
