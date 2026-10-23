# Uploads

A file is written whole or not at all. `put` writes one in a call, from bytes,
text or a stream; `upload` writes one a piece at a time, for bytes that arrive
over time, and publishes it only at `commit`. Nobody ever sees half a file:
not while it is written, not after a crash.

```ts
const reports = store.files('reports', { maxFileSize: '2GiB' })

await reports.put('2026-10/summary.txt', 'Revenue: 12,400 EUR') // FileInfo
await reports.put('2026-10/data.csv', request.body!, { contentType: 'text/csv', size: Number(request.headers.get('content-length')) })
```

```python
reports = store.files("reports", max_file_size="2GiB")

await reports.put("2026-10/summary.txt", "Revenue: 12,400 EUR")  # FileInfo
await reports.put("2026-10/data.csv", request.stream(), content_type="text/csv", size=int(request.headers["content-length"]))
```

```go
reports, err := blobs.OpenFiles(store, "reports", blobs.MaxFileSize(2<<30))

_, err = reports.Put(ctx, "2026-10/summary.txt", strings.NewReader("Revenue: 12,400 EUR"))
_, err = reports.Put(ctx, "2026-10/data.csv", r.Body, blobs.ContentType("text/csv"), blobs.Size(r.ContentLength))
```

```rust
let reports = store.files("reports").max_file_size(2 << 30).open()?;

reports.put("2026-10/summary.txt", b"Revenue: 12,400 EUR")?;
reports.key("2026-10/data.csv").content_type("text/csv").size(length).put(&body)?;
```

`put` replaces whatever the path held, at the moment it returns. What it
takes:

| Language   | Bytes, text or a stream                                                                                                   |
|------------|---------------------------------------------------------------------------------------------------------------------------|
| TypeScript | `string`, `Uint8Array`, `ArrayBuffer`, `Blob` (a `File`, `Bun.file`), `ReadableStream`, an async iterable of `Uint8Array` |
| Python     | `bytes`, `str`, a file object, an async iterator of `bytes`                                                               |
| Go         | an `io.Reader`                                                                                                            |
| Rust       | bytes; an `upload` for a reader                                                                                           |

## An upload a piece at a time

```ts
await using upload = await reports.upload('2026-10/report.csv', { contentType: 'text/csv' })
for await (const chunk of rows) await upload.write(chunk)
await upload.commit() // the file appears now; leaving the block without commit aborts
```

```python
async with reports.upload("2026-10/report.csv", content_type="text/csv") as upload:
    async for chunk in rows:
        await upload.write(chunk)
    await upload.commit()  # leaving the block without it aborts
```

```go
upload, err := reports.Upload(ctx, "2026-10/report.csv", blobs.ContentType("text/csv"))
defer upload.Abort() // does nothing after Commit
_, err = io.Copy(upload, rows)
info, err := upload.Commit(ctx)
```

```rust
let mut upload = reports.key("2026-10/report.csv").content_type("text/csv").upload()?;
std::io::copy(&mut rows, &mut upload)?;
let info = upload.commit()?; // dropped without it, the upload aborts
```

What an upload promises, step by step:

1. Its bytes go to a place of their own as they come; their hash and size are
   counted on the way. Nothing a reader can see changes.
2. `commit` checks the size, syncs the bytes and publishes the file in one
   commit of the store. Before it returns, readers see what the path held;
   after, the new file; never part of it (contract BLOB-01).
3. An upload aborted, dropped, or left by a process that died is no file. Its
   bytes are removed at once, or, after a crash, when the store next opens.
   On Windows, bytes that another program such as a virus scanner holds then
   are removed in the background once it lets them go, and the store opens.
4. A commit whose outcome is unknown publishes nothing it cannot account for:
   the next open removes bytes no file names.

An upload's memory does not grow with the file: a few buffers of 64 KiB to
1 MiB, as the store's memory allows. A `put` with a stream is an upload in
one call.

## Sizes

| Option        | Says                                                                         |
|---------------|------------------------------------------------------------------------------|
| `size`        | how long this body is, from `Content-Length`: longer or shorter is `invalid` |
| `maxFileSize` | the largest file the set takes: past it is `limit`                           |

```text
files reports: "2026-10/data.csv": a body of 5 bytes, shorter than its size of 10: invalid
files reports: "huge.bin": a file of 2097152 bytes, past the files' bound of 1048576: limit
```

Either way nothing is written. `maxFileSize` bounds one file and is not a
quota: nothing already stored is removed to make room, and there is no bound
unless set. A size is a number of bytes or text in `KiB`, `MiB`, `GiB` or
`TiB`; `'2GB'` is `invalid`, since it is 10⁹ bytes to some and 2³⁰ to others.

The store keeps 1 GiB of the disk free unless told otherwise: `keep_free` in
Rust's `Options`, `tinystore serve --keep-free 10GiB` for a server. A file that would leave less
is `limit` and leaves nothing; an upload asks the disk as its first bytes
arrive and every 64 MiB after.

## What a file carries

```ts
await reports.put('invoice.pdf', pdf, { contentType: 'application/pdf', meta: { name: 'Счёт №42.pdf' } })
```

```python
await reports.put("invoice.pdf", pdf, content_type="application/pdf", meta={"name": "Счёт №42.pdf"})
```

```go
_, err = reports.Put(ctx, "invoice.pdf", bytes.NewReader(pdf), blobs.ContentType("application/pdf"),
	blobs.Meta(map[string]string{"name": "Счёт №42.pdf"}))
```

```rust
reports.key("invoice.pdf").content_type("application/pdf").meta("name", "Счёт №42.pdf").put(&pdf)?;
```

| Field          | Meaning                                                              |
|----------------|----------------------------------------------------------------------|
| `path`         | its path, relative to the folder it was asked from                   |
| `size`         | bytes                                                                |
| `etag`         | the SHA-256 of its bytes in hex, quoted: an HTTP `ETag` as it is     |
| `contentType`  | the type it was written with, `application/octet-stream` without one |
| `lastModified` | when this version was written                                        |
| `expires`      | when it expires, if it does                                          |
| `meta`         | a few strings of yours, 2 KiB in all: the name it was uploaded as    |

A file carries what serving it needs. Its owner, album or caption are rows of
your database that name the file by its path.

## Folders

```ts
const media = store.files('media')
await media.folder('users', 42).put('photos/1.jpg', photo) // the file users/42/photos/1.jpg
const page = await media.folder('users', 42).list({ prefix: 'photos/' }) // { files, next }
const { count, size } = await media.folder('users', 42).usage()
await media.folder('users', 42).clear() // every file of user 42, at once
```

```python
media = store.files("media")
await media.folder("users", 42).put("photos/1.jpg", photo)
page = await media.folder("users", 42).list(prefix="photos/")
usage = await media.folder("users", 42).usage()
await media.folder("users", 42).clear()
```

```go
media, err := blobs.OpenFiles(store, "media")
_, err = media.Folder("users", 42).Put(ctx, "photos/1.jpg", photo)
page, err := media.Folder("users", 42).List(ctx, blobs.Prefix("photos/"))
usage, err := media.Folder("users", 42).Usage(ctx)
err = media.Folder("users", 42).Clear(ctx)
```

```rust
let media = store.files("media").open()?;
media.folder("users").folder(42).key("photos/1.jpg").put(&photo)?;
let page = media.folder("users").folder(42).list().prefix("photos/").page()?;
let usage = media.folder("users").folder(42).usage()?;
media.folder("users").folder(42).clear()?;
```

- Each argument of `folder` is one whole segment: `folder('users', 42)` never
  holds `users/420`'s files. An empty argument is `invalid`, so a missing id
  never names `users/` and its `clear` never empties every user's.
- A `prefix` in `list` is text: `prefix: 'photos/1'` finds `photos/10.jpg`
  too.
- `list` reads a page of at most 1,000 files, sub-folders included, in the
  byte order of the paths; `all()` walks every page.
- `clear` removes a folder however many files it holds, at once for every
  reader; the bytes go in the background. Keep a user's files in a folder of
  theirs and deleting an account is one call.

## Paths

A path is segments between `/`: none empty, none `.` or `..`, at most 1 KiB
and 16 segments. Anything else is `invalid`, so no path leaves its folder.
Paths name nothing on disk, since bytes are stored under numbers: `Photo.jpg`
and `photo.jpg` are two files on every system, and `CON` is a path like any
other (contract BLOB-08).

## See also

- [Blobs](README.md): the engine's overview.
- [Serving files](serving.md): handing a file to a browser.
- [Attachments](../../handbook/chat/attachments.md) in the Handbook.
