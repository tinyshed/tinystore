# Integrity

TinyStore computes a SHA-256 hash of every file while it is uploaded. When you
read a file in full, it checks the bytes against that hash, and a background
scrub reads every file once a month. A changed byte on the disk is detected
instead of being served to your users.

## A changed file fails before its last byte

```ts
try {
	const bytes = await (await files.get('invoices/2026-10.pdf'))!.bytes()
} catch (err) {
	if (err instanceof CorruptError) {
		// the file on disk no longer matches what was stored: restore it from a backup
	}
}
```

```python
try:
    data = await (await files.get("invoices/2026-10.pdf")).read()
except CorruptError:
    ...  # the file on disk no longer matches what was stored: restore it from a backup
```

```go
file, found, err := files.Open(ctx, "invoices/2026-10.pdf")
defer file.Close()
_, err = io.Copy(w, file)
if errors.Is(err, tinystore.ErrCorrupt) {
	// the file on disk no longer matches what was stored: restore it from a backup
}
```

A reader that reads a file from its first byte to its last, in order, checks
the hash before it hands over the last bytes. If the bytes changed, the read
fails with a corrupt error (`ErrCorrupt`, `CorruptError`). An HTTP client then
receives fewer bytes than the `Content-Length` promised, and treats the
download as failed.

Small files, up to 16 KiB, are stored inside the database and checked before
their first byte.

## Ranges are not checked

A range read returns the bytes as the disk has them, because checking the hash
would require reading the whole file. The scrub covers ranges: it finds a
changed byte anywhere in a file within its next pass.

## The scrub

TinyStore reads every stored file once every 30 days, in small slices: each
minute it reads 1/43,200 of the stored bytes, and at least 1 MiB, so a small
store is checked within an hour. The scrub remembers where it stopped, so a
restart doesn't start it over.

When the scrub finds a changed or missing file, it marks the file, logs an
error with the keys of every file that uses those bytes, and the next read of
those keys fails with a corrupt error. Writing new bytes to the key, deleting
it or clearing its folder works as usual. A backup is what repairs it.

## Plain files on disk

A file stored on disk contains exactly the object's bytes, without a header.
`sha256sum`, `ffprobe` or a hard link read it as it is. A file is never
changed after it is stored, and never renamed after it is moved into place,
which makes it safe to copy with rsync or restic while the store runs. See
[Backups](../running/backups.md) for a consistent copy of the whole store.

## Limits and defaults

|             |                                                        |
|-------------|--------------------------------------------------------|
| Hash        | SHA-256, computed during the upload                    |
| Whole reads | checked                                                |
| Range reads | not checked                                            |
| Scrub       | every file once per 30 days, at least 1 MiB per minute |

## See also

- [blobs/README.md](../../blobs/README.md): the full contract of integrity.
- [design/blobs.md](https://github.com/tinyshed/research/blob/main/tinystore/design/blobs.md)
  in the research repository: crash consistency and how files are written.
