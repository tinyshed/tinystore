# blobs

An application's files, in `blobs/` inside a `tinystore.Store`: avatars and
their sizes, a chat's attachments, a camera's recordings, an export a user
downloads tomorrow, an editor's documents. Objects sit under keys that are
paths, their rows in `blobs.db`, their bytes inline up to 16 KiB and in a file
of their own above it. The design and the measurements behind it are
research's [design/blobs.md](https://github.com/tinyshed/research/blob/main/tinystore/design/blobs.md) and
[the mechanics round](https://github.com/tinyshed/research/blob/main/tinystore/reports/blobs-mechanics-2026-09-27.md).

```go
objects, err := blobs.Open(ctx, store, blobs.Options{}) // data/blobs/

avatars, err := blobs.OpenBucket(ctx, objects, "avatars", blobs.MaxSize(20<<20))
exports, err := blobs.OpenBucket(ctx, objects, "exports", blobs.DefaultTTL(24*time.Hour))

obj, err := avatars.Of(user.ID).Put(ctx, "original", r.Body,
	blobs.ContentType(r.Header.Get("Content-Type")), blobs.Size(r.ContentLength))
photo, found, err := avatars.Of(user.ID).Open(ctx, "original") // Read, ReadAt, Seek, Close
http.ServeContent(w, r, "", photo.Modified, photo)             // Range, If-Range, If-None-Match, HEAD

upload, err := exports.Of(userID).Create(ctx, "notes.zip", blobs.ContentType("application/zip"))
defer upload.Abort()
_, err = io.Copy(upload, archive)
export, err := upload.Commit(ctx)

_, err = files.Of("users", 7).Move(ctx, "pending/u1", "sent/u1") // one write, the bytes untouched
usage, err := files.Of("users", 7).Usage(ctx)                    // counted from the rows
err = avatars.Of(user.ID).Clear(ctx)                             // the account deleted
```

[example_test.go](example_test.go) runs the five cases of
[design/blobs.md](https://github.com/tinyshed/research/blob/main/tinystore/design/blobs.md) as examples, and `go doc` shows them.

## Contracts

- **A key is a path.** Segments joined by `/`, each non-empty, not `.` or
  `..`, UTF-8 without a control character; at most 16 segments and 1 KiB with
  the owners. It names no file, so `Photo.jpg` and `photo.jpg` are two keys on
  every file system and a name Windows reserves is a key like any other.
  `Of(owners...)` names a folder a segment an owner, a string or an integer,
  an integer by its decimal text; `Of("users", 4)` never meets `users/42/`. An
  object's `Key` is its path under the handle that returned it. Anything else
  is `tinystore.ErrInvalid`.
- **An object appears at its commit, whole, or not at all.** `Put` returns
  once the object is durable: up to 16 KiB its bytes are a row that commits
  beside the writes of other goroutines; above it they are a file, written in
  `uploads/`, synced, renamed into `objects/`, its directory synced, and only
  then named by the row. Whatever the key held is replaced at that commit. A
  file is never written again nor given another name.
- **Memory does not follow size.** An upload holds 16 KiB of the store's
  memory while it may yet be inline; past it the bytes go to its file as they
  arrive, synced every 256 MiB. After 256 KiB, `Put` tries a 64 KiB transfer
  buffer once, reserving both buffers during the change (80 KiB). If that
  memory is not free immediately, it keeps the 16 KiB buffer without waiting.
  `Create`/`Write` needs only the initial buffer. `Size(n)` says the stream's
  length; a shorter or longer stream is `ErrInvalid`. A `*bytes.Reader`, `*bytes.Buffer`
  or `*strings.Reader` says its own. `MaxSize(n)` bounds a bucket's objects and
  `Options.KeepFree` what an upload leaves free on the disk, 1 GiB unless it
  says, checked at the start for a declared size and every 64 MiB; either
  stops the upload with `ErrLimit`.
- **An upload that does not commit leaves nothing.** `Abort`, a failed
  `Write`, a stream that fails, the end of the context given to `Create` and
  the store's `Close` each remove its bytes. A call after its context ended
  finds it aborted, and maintenance aborts one left without calls.
- **A condition is checked twice.** `IfNoneMatch()` creates only; `IfMatch`
  takes an `If-Match` header as it is, a list of ETags or `*`, and writes or
  deletes only while the key holds a live object with one of them. Both are
  checked when an upload begins, so that a doomed one stops before its first
  byte, and in the commit, which decides: `ErrConflict`.
- **The ETag is the bytes'.** It is quoted, as a header carries it, and
  derives from the SHA-256 computed as the bytes arrive, so equal bytes have
  one ETag; its derivation is not promised.
- **A reader keeps what it opened.** `Open` returns the object as it is at
  that moment: a reader of a file holds the file, opened so that it shares its
  deletion on Windows, and reads on after its key is deleted, replaced,
  expired or cleared and after the store closes, until its own `Close`. A file
  gone between the lookup and the open means its key changed: the key is
  looked up again, three times at most, then `ErrConflict`; a file missing
  while its key names it is `ErrCorrupt`.
- **A whole read is checked, a range is not.** An inline object is checked
  against its SHA-256 before its first byte. A file read from its first byte
  to its last in order, as `io.Copy` and `http.ServeContent` read it, fails
  with `ErrCorrupt` instead of handing over its last bytes when they do not
  hash to the object's. `ReadAt`, and a `Read` after a `Seek` elsewhere, read
  their bytes as the file has them.
- **The scrub reads every content once a pass.** Each `Maintain` reads a
  43,200th of the contents' bytes and at least 1 MiB, a pass of 30 days at a
  call a minute, and keeps its place and its hash's state in `blobs.db`. A
  content changed or missing is marked, logged once at Error with the keys
  naming it, and its next `Open` is `ErrCorrupt`; writes over it work as over
  any other, and a backup is what repairs it.
- **`Copy` shares the bytes.** The destination is a new version naming the
  source's content, with its type and meta unless the call gives others; no
  byte is read or written. `Move` is `Copy` and the source's removal in one
  commit. Both stay inside a bucket, take `IfNoneMatch` or `IfMatch` for the
  destination, and refuse an absent source with `ErrConflict`. Bytes leave
  once no key names them: a file right after the commit that dropped its last
  name, and at the next maintenance if its removal failed.
- **Every write gives its object the expiry a `Put` would**: the call's `TTL`
  or `ExpireAt`, else the bucket's `DefaultTTL` from the write, else none. An
  expired object is absent to every call, and maintenance removes it, 10,000
  a transaction.
- **`Scan` walks every key under a folder**, the folders under it included,
  in the byte order of the paths, `10.jpg` before `9.jpg`: 1000 objects a page
  at most, each page one statement's snapshot; `All` walks the pages holding
  none between them. `Usage` counts the objects and bytes under a folder from
  their rows, a millisecond over ten thousand; it is a check and not a limit.
- **`Clear` removes a folder and every folder under it at once.** Up to
  10,000 objects go in one transaction; a larger folder is marked cleared at
  the file's revision, which hides its objects from every call the moment it
  commits, and maintenance deletes them. An object written after it stays.
- **A snapshot links the files.** `Store.Snapshot`, and so `backup.Write`,
  copies `blobs.db` and hard-links every file its copy names, a copy where
  links are refused; removal waits meanwhile. Each file is its own entry of
  the backup, stored rather than deflated.
- **An error names its object.** A call refused because of one object is a
  `*blobs.KeyError` with the bucket and the whole path; `errors.Is` finds the
  store's sentinel in it. A commit whose outcome is unknown is
  `blobs.ErrOutcomeUnknown`: the object may or may not be there, and its
  caller `Stat`s it before writing again.

## Testing without waiting

A Manual store with a clock the test moves runs no background work, so expiry,
the scrub and the removal of what uploads left happen when the test says:

```go
now := time.Now()
store, err := tinystore.Open(ctx, t.TempDir(), tinystore.Options{Manual: true, Clock: func() time.Time { return now }})
objects, err := blobs.Open(ctx, store, blobs.Options{})
exports, err := blobs.OpenBucket(ctx, objects, "exports", blobs.DefaultTTL(24*time.Hour))

_, err = exports.Put(ctx, "notes.zip", bytes.NewReader(archive))
now = now.Add(25 * time.Hour)
_, found, err := exports.Open(ctx, "notes.zip") // false: expired
_, err = objects.Maintain(ctx)                  // removes it and its file, and scrubs a slice
```

## Not in the first version

What [design/blobs.md](https://github.com/tinyshed/research/blob/main/tinystore/design/blobs.md) leaves for later: resuming an upload,
packs for small files, totals kept for folders, checked ranges, sharing the
bytes of equal uploads, compression, a folder's own objects and sub-folders,
versions kept after a replace, a quota enforced in the commit, transactions
over several calls, appends.
