# Integrity

Two things can go wrong with a stored file: its bytes change on the disk, or
two writers overwrite each other. A whole read is checked against the file's
SHA-256, a scrub reads every file in the background, and `ifMatch` and
`create` keep two writes from crossing.

## Checked reads

```ts
const file = await docs.get('contract.pdf')
try {
	const bytes = await file!.bytes() // hashed as it is read
} catch (err) {
	if (err instanceof CorruptError) return restoreFromBackup('contract.pdf')
	throw err
}
```

```python
file = await docs.get("contract.pdf")
try:
    data = await file.read()  # hashed as it is read
except tinystore.CorruptError:
    return await restore_from_backup("contract.pdf")
```

```go
file, found, err := docs.Get(ctx, "contract.pdf")
defer file.Close()
data, err := io.ReadAll(file) // hashed as it is read
var e *tinystore.Error
if errors.As(err, &e) && e.Kind == tinystore.Corrupt {
	return restoreFromBackup("contract.pdf")
}
```

```rust
let mut file = docs.get("contract.pdf")?.expect("contract.pdf");
match file.read_all() {
    Err(err) if err.kind() == ErrorKind::Corrupt => restore_from_backup("contract.pdf"),
    other => other.map(|_| ()),
}?;
```

A whole read hashes the bytes as they come and compares them with the hash
taken when they were written. The last bytes are held back until they match,
so a file whose bytes changed fails `corrupt` before its end, and a whole
body is never handed over as good (contract BLOB-02).

A response that is already streaming has sent its status and its first bytes
when that happens: your handler ends it on the error, and the client sees a
body cut short, never a whole wrong one.

A range, `slice` or a seek, is not checked: only a read from the first byte
to the last is.

## The scrub

In the background the store reads every stored file over 30 days, a slice a
minute, and checks each against its hash. A file whose bytes changed is said
in the store's log, once, with every path that names it:

```text
blobs: a file's bytes are changed  content=4471 paths=["docs/contracts/2026/acme.pdf"]
```

A file missing from the disk, or shorter than its size, is said the same way.
With the store's own metrics on, `tinystore_blobs_damaged_total` counts them.

A disk that rots a byte is found within a month, whether or not anyone reads
the file, while last month's backup still holds it whole.

## Write only if nobody changed it

`ifMatch` takes the ETag a read gave: if the file changed, expired or was
deleted since, the write is `conflict` and nothing is written.

```ts
const doc = await docs.head('notes.md')
await docs.put('notes.md', edited, { ifMatch: doc!.etag }) // ConflictError if someone saved first
```

```python
doc = await docs.head("notes.md")
await docs.put("notes.md", edited, if_match=doc.etag)
```

```go
doc, found, err := docs.Head(ctx, "notes.md")
_, err = docs.Put(ctx, "notes.md", strings.NewReader(edited), blobs.IfMatch(doc.ETag))
```

```rust
let doc = docs.head("notes.md")?.expect("notes.md");
docs.key("notes.md").if_match(&doc.etag).put(&edited)?;
```

Of two writes that read the same ETag, one conflicts (contract BLOB-04). The
editor that loses reads the file again and decides: merge, ask the user, or
save as a copy. `ifMatch` works on `put`, `upload`, `delete`, `copy` and
`rename`.

## Write only if there is none

```ts
const created = await docs.create('invoices/2026-0042.pdf', pdf) // false when a file was there
```

```python
created = await docs.create("invoices/2026-0042.pdf", pdf)
```

```go
created, err := docs.Create(ctx, "invoices/2026-0042.pdf", bytes.NewReader(pdf))
```

```rust
let created = docs.create("invoices/2026-0042.pdf", &pdf)?;
```

`create` writes only when the path holds no file and says whether it did, as
kv's `create` does: a file already there is an answer, not an error. An
invoice numbered twice keeps the first.

## Errors

| Error             | When                                                                                                                  | Do                                   |
|-------------------|-----------------------------------------------------------------------------------------------------------------------|--------------------------------------|
| `invalid`         | a bad name, path, folder or size; a body longer or shorter than its `size`                                            | fix the call                         |
| `not found`       | `copy` or `rename` from a path with no file                                                                           | check the path                       |
| `conflict`        | `ifMatch` did not match; `copy` or `rename` onto a file; over a connection, a read of a file replaced since its `get` | read again and decide                |
| `limit`           | past `maxFileSize`; a write that would leave the disk less than the store keeps free                                  | write less, or free the disk         |
| `corrupt`         | a whole read whose bytes do not match their hash                                                                      | restore the file from a backup       |
| `outcome unknown` | the commit failed after the write ran                                                                                 | `head` the path before writing again |

Every error names the files and the path: `files docs: "notes.md": conflict`.
`get` and `head` of a path with no file, and `delete` of one, are not errors.

## Why this design?

**Why hash on every whole read.** A disk, a cable or a controller can change
a byte, and a filesystem rarely notices. Without a check, the program serves
the damaged file as good, and the backup made tonight copies the damage.
SHA-256 is computed while the bytes stream anyway, so the check costs the
hash and nothing else.

**Why hold the last bytes back.** A check that runs after the last byte is
sent tells the program too late: the client already has a whole, wrong file.
Holding back the end until the hash matches turns damage into a body cut
short, which every client treats as a failed download.

## See also

- [Blobs](README.md): the engine's overview.
- [Backup](../backup/README.md): a backup of files checks every byte when it
  is restored.
