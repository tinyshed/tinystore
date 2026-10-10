# blobs: the API book

Draft, 10 October 2026, after a newcomer check, a blind comparison of names
and a review (below). The application's files: avatars and their sizes, a
chat's attachments, a camera's recordings, an export a user downloads
tomorrow, a backup. This book is the API before the code; [dx.md](../dx.md)
has the rules it follows. TypeScript comes first in each section because most
callers will read it; Python, Go and Rust follow when they spell something
differently. The engine is `blobs`: its module, its directory and its
documents use the word, and a program never needs it. The Go engine at
`e81a050` and research's [design/blobs.md][design] are what this ports: its
promises are GATES.md's, its storage was measured.

[design]: https://github.com/tinyshed/research/blob/main/tinystore/design/blobs.md

## What a newcomer learns

| Concept             | In one line                                                                                     |
|---------------------|-------------------------------------------------------------------------------------------------|
| files               | a named set of files by path, as `store.files('avatars')` opens it                              |
| path                | text such as `users/42/photos/1.jpg`: segments between `/`, the file's name everywhere          |
| `folder`            | the files under a path, such as one user's, listed, counted and cleared in one call             |
| a file              | bytes written whole and replaced whole, with a type, an ETag and a few strings of the program's |
| an upload           | a file written a piece at a time, which appears at `commit` and never before                    |
| `ifMatch`, `create` | write only if nobody changed the file since you read it, or only if there is none               |
| `ttl`               | a file expires a span after it was written                                                      |

Everything opens from the store, named by what it is: `store.files`, beside
`store.bucket` and `store.queue`. A file's owner, album or caption are rows
of the application's database or values of kv, which name the file by its
path: a file carries what serving it needs and nothing a query would search.

## Open files

```ts
const avatars = store.files('avatars')
const reports = store.files('reports', { ttl: '24h', maxFileSize: '2GiB' })
```

```python
avatars = store.files("avatars")
reports = store.files("reports", ttl="24h", max_file_size="2GiB")
```

```go
avatars, err := blobs.Files(store, "avatars")
reports, err := blobs.Files(store, "reports", blobs.TTL(24*time.Hour), blobs.MaxFileSize(2<<30))
```

```rust
let avatars = store.files("avatars").open()?;
let reports = store.files("reports").ttl(Duration::from_hours(24)).max_file_size(2 << 30).open()?;
```

- A name is `[a-z0-9][a-z0-9_-]{0,63}`. Its options are the program's and may
  change between runs.
- `ttl` is the term of every file written without its own, not a ceiling: a
  write that gives its own term keeps it.
- `maxFileSize` bounds one file. A write past it is refused and leaves
  nothing; nothing already stored is ever removed to make room. There is no
  bound by default, since a film is 17 to 80 GB, and no quota over a folder:
  `usage` counts, it does not limit.
- A size is a number of bytes, or text in `KiB`, `MiB`, `GiB` or `TiB`.
  `'2GB'` is `invalid`: it is 10⁹ bytes to some and 2³⁰ to others.

## Write and read

```ts
await avatars.put(`${user.id}.png`, png, { contentType: 'image/png' })   // replaces any file there; FileInfo
const avatar = await avatars.get(`${user.id}.png`)                         // StoredFile, undefined when there is none
const bytes = avatar ? await avatar.bytes() : undefined                    // the bytes as they were at get
const info = await avatars.head(`${user.id}.png`)                          // FileInfo, undefined when there is none
await avatars.delete(`${user.id}.png`)                                     // nothing to delete is not an error
```

```python
await avatars.put(f"{user.id}.png", png, content_type="image/png")
avatar = await avatars.get(f"{user.id}.png")   # StoredFile | None
data = await avatar.read() if avatar else None
info = await avatars.head(f"{user.id}.png")    # FileInfo | None
await avatars.delete(f"{user.id}.png")
```

```go
info, err := avatars.Put(ctx, id+".png", r, blobs.ContentType("image/png"))
avatar, found, err := avatars.Get(ctx, id+".png") // *blobs.StoredFile: io.ReadSeekCloser, io.ReaderAt
defer avatar.Close()
info, found, err := avatars.Head(ctx, id+".png")
err = avatars.Delete(ctx, id+".png")
```

```rust
avatars.key(&format!("{id}.png")).content_type("image/png").put(&png)?;  // FileInfo
let avatar: Option<StoredFile> = avatars.get(&format!("{id}.png"))?;    // Read + Seek
let bytes = avatar.map(|mut file| file.bytes()).transpose()?;
let info: Option<FileInfo> = avatars.head(&format!("{id}.png"))?;
avatars.delete(&format!("{id}.png"))?;
```

- `put` writes the file and replaces whatever the path held, at the moment
  it returns. Nobody sees half of a file, while it is written or after a
  crash. It returns once the file is on disk as the store's durability says:
  with `'full'`, the default, it survives a power loss, and with `'os'` a
  crash of the process.
- What `put` takes is bytes, text or a stream: in TypeScript a `string`,
  `Uint8Array`, `ArrayBuffer`, `Blob` (a `File` or `Bun.file` among them),
  `ReadableStream` or async iterable of `Uint8Array`; in Python `bytes`, `str`,
  a file object or an async iterator of `bytes`; in Go an `io.Reader`; in Rust
  bytes, and an `upload` for a reader.
- `get` returns the file and the bytes it will read, which stay what they were
  when `get` returned, through a replace, a delete or an expiry, on Windows
  too. In TypeScript a `StoredFile` reads as a web `Blob` does, once:
  `stream()`, `bytes()`, `text()`, `json()`, `arrayBuffer()`, and
  `slice(start, end)` with the end left out, as `Blob.slice` leaves it. In
  Python `read()` and `async for chunk in file`; in Go and Rust a reader.
- `bytes()`, `text()` and `json()` hold the whole file in memory; a large one
  is read with `stream()`.
- `head` reads what a file carries and none of its bytes.

## What a file carries

| Field          | Meaning                                                                         |
|----------------|---------------------------------------------------------------------------------|
| `path`         | its path, relative to the folder it was asked from                              |
| `size`         | bytes                                                                           |
| `etag`         | the SHA-256 of its bytes in hex, quoted: `"9f86d081…"`, an HTTP `ETag` as it is |
| `contentType`  | the type it was written with, `application/octet-stream` without one            |
| `lastModified` | when this version was written                                                   |
| `expires`      | when it expires, if it does                                                     |
| `meta`         | a few strings of the program's, such as the name it was uploaded as             |

The ETag depends on the bytes alone: two files with the same bytes have the
same one, and a copy kept anywhere else is checked against it.

## Paths and folders

```ts
const media = store.files('media')

await media.folder('users', 42).put('photos/1.jpg', photo)   // the file users/42/photos/1.jpg
await media.put('users/42/photos/1.jpg', photo)              // the same file
const page = await media.folder('users', 42).list({ prefix: 'photos/' })   // { files, next }
for await (const file of media.folder('users', 42).all()) { … }            // FileInfo, a page at a time
const { count, size } = await media.folder('users', 42).usage()
await media.folder('users', 42).clear()                                    // every file of user 42
```

```rust
media.folder("users").folder(42).key("photos/1.jpg").put(&photo)?;
let page = media.folder("users").folder(42).list().prefix("photos/").page()?;   // Page { files, next }
let usage = media.folder("users").folder(42).usage()?;                         // Usage { count, size }
media.folder("users").folder(42).clear()?;
```

- A path is segments between `/`: none empty, none `.` or `..`, no NUL, at
  most 1 KiB and 16 segments with its folders. A path that is not one is
  `invalid`, so no path leaves the folder it was written in.
- `folder('users', 42)` is the folder `users/42`: each argument is one
  segment, and a file's path in it is the folder's segments and then its own,
  so the two writes above are one file. A folder exists while it holds files,
  and needs no creating or deleting.
- `folder('users', 4)` never holds user 42's files, nor `users/420`'s: a
  folder is whole segments, not a prefix of text. An argument that is empty,
  has a `/` or is `.` or `..` is `invalid`, so a missing id never names
  `users/` and its `clear` never empties every user's.
- A `prefix` in `list` is text within the folder: `prefix: 'photos/1'` finds
  `photos/10.jpg` too.
- A path names nothing on disk, since files are stored under numbers:
  `Photo.jpg` and `photo.jpg` are two files on every operating system, and
  `CON` is a path like another.
- `list` reads one page, at most 1000 files, every file in the folder and in
  the folders in it, in the byte order of the paths; `next` is where the
  following page starts. `all` walks every page, holding nothing between
  them.
- `usage` counts the files and their bytes in a folder as it is now. Two
  writes that each passed a check of it can together pass what it was checked
  against.
- `clear` removes a folder and every folder in it, at once for every reader,
  however many files it holds. On the files themselves it empties them. Keep
  a user's files in a folder of theirs and deleting an account is one call.
- kv's `under` is another thing: a branch whose owners are kept apart from
  the key's text, where a folder's segments and its files' paths are one path.

## Uploads

```ts
await using upload = await reports.upload('2026-10/report.csv', { contentType: 'text/csv' })
for await (const chunk of rows) await upload.write(chunk)
await upload.commit()      // FileInfo; the file appears now. Leaving the block without it aborts.
```

```python
async with reports.upload("2026-10/report.csv", content_type="text/csv") as upload:
    async for chunk in rows:
        await upload.write(chunk)
    await upload.commit()   # leaving the block without it aborts
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
let info = upload.commit()?;   // dropped without it, the upload aborts
```

What an upload promises, step by step:

1. Its bytes go to a place of their own as they come, and their hash and size
   are counted on the way. Nothing a program can see changes.
2. `commit` checks the size against `size`, when one was given, and
   `maxFileSize`, syncs the bytes and publishes the file in one commit of the
   store, replacing what the path held as `put` does. Before it returns,
   readers see what the path held; after, the new file; never part of it.
3. An upload aborted, dropped or left by a process that died is no file. Its
   bytes are removed at once, or, after a crash, when the store next opens,
   which looks only where an unfinished upload can have left them. They count
   in no folder's usage.
4. A commit whose outcome is unknown, `outcome unknown`, publishes nothing it
   cannot account for: the next open removes bytes no file names.

- `put` with a stream is an upload in one call.
- `size` declares how long the body is, from an HTTP `Content-Length` for
  instance; a body longer or shorter is refused and leaves nothing.
- An upload does not resume after its process ends, in the first version.
- Its memory does not follow the file's size: a few buffers of 64 KiB to
  1 MiB, as the store's memory allows.

## Conditions

```ts
const doc = await docs.head('notes.md')
if (doc) await docs.put('notes.md', text, { ifMatch: doc.etag })   // ConflictError if it changed since
const created = await docs.create('notes.md', text)                 // false when a file was there
```

```rust
docs.key("notes.md").if_match(&doc.etag).put(&text)?;   // a Conflict error if it changed
let created: bool = docs.create("notes.md", &text)?;
```

- `ifMatch` takes the ETag a read gave: a file that changed, expired or was
  deleted since is a conflict, and nothing is written. It works on `put`,
  `upload`, `delete`, `copy` and `rename`, for the path written.
- `create` writes only when the path holds no file, and says whether it did,
  as kv's `create` says; a file there is never an error. It takes what `put`
  takes, a stream among it.
- Of two writes that read the same ETag, one conflicts.

## Copies and renames

```ts
await docs.copy('notes.md', 'notes (copy).md')                // FileInfo; no bytes are copied
await docs.rename('draft.md', 'final.md')                     // ConflictError if final.md is there
await docs.rename('draft.md', 'final.md', { ifMatch: final.etag })   // replaces that version of it
```

```rust
docs.copy("notes.md", "notes (copy).md")?;
docs.rename("draft.md", "final.md")?;
docs.key("final.md").if_match(&final_.etag).rename_from("draft.md")?;
```

- `copy` and `rename` never replace a file by surprise: a path that holds one
  is a conflict, unless `ifMatch` names the version to replace. `put` and an
  upload are writes of a path and replace what it held; these move a file
  that exists onto one that may.
- A copy and its source are two files from then on: replacing or deleting one
  leaves the other. It costs a row, whatever the size, since stored bytes are
  never changed and are shared until the last file that names them goes.
- `rename` is a copy and a delete in one write; the source gone is
  `not found`. Both stay within one `files`; between two, read and `put`.

## Expiry

```ts
await reports.put('report.csv', csv)                       // the files' ttl, 24h here, from now
await reports.put('report.csv', csv, { ttl: '7d' })        // its own
await reports.expire('report.csv', '1h')                   // from now; a Date for a time
```

```rust
reports.key("report.csv").ttl(Duration::from_hours(24 * 7)).put(&csv)?;
reports.expire("report.csv", Duration::from_hours(1))?;
```

- A term counts from the write, and every write gets its own: an export
  written again lives its day again. An expired file is gone for readers at
  once and its bytes are removed in the background. A backup does not belong
  in files with a `ttl`.

## Serving files

```ts
Bun.serve({
  async fetch(request) {
    const user = await signedIn(request)                     // the program's own
    const file = await avatars.get(`${user.id}.png`)
    if (!file) return new Response('not found', { status: 404 })
    if (request.headers.get('if-none-match') === file.etag) return new Response(null, { status: 304 })
    return new Response(file.stream(), {
      headers: { 'content-type': file.contentType, etag: file.etag, 'content-length': String(file.size) },
    })
  },
})
```

```go
file, found, err := avatars.Get(ctx, path)
defer file.Close()
http.ServeContent(w, r, path, file.LastModified, file) // Range, If-None-Match and If-Modified-Since
```

- The engine knows no HTTP, and checks no one's right to a file: which path
  a request may read is the program's, as the example's `signedIn` is.
- A content type a user gave is the user's: an upload served as `text/html`
  or `image/svg+xml` from the program's own origin runs its scripts there.
  Serve uploads with `content-disposition: attachment`, from another origin,
  or with a type the program chose.
- A range is `file.slice(start, end)` in TypeScript and a seek in the others,
  and it is not checked.

## Integrity

- A read of a whole file is checked: the bytes are hashed as they are read
  and compared with the hash taken when they were written, and the last bytes
  are held back until they match. A file whose bytes changed fails as
  `corrupt` before its end, so a whole body is never handed over as good.
- A response already streaming when that happens has sent its status and its
  first bytes: the program ends it on the error, and the client sees a body
  cut short, never a whole one.
- A range is not checked in the first version.
- In the background a scrub reads every stored file over 30 days and names
  the paths whose bytes changed.

## Errors

| Error             | When                                                                                 | What to do                           |
|-------------------|--------------------------------------------------------------------------------------|--------------------------------------|
| `invalid`         | a bad name, path, folder or size; a body longer or shorter than its `size`           | fix the call                         |
| `not found`       | `copy` or `rename` from a path with no file                                          | check the path                       |
| `conflict`        | `ifMatch` did not match; `copy` or `rename` onto a file without it                   | read again and decide                |
| `limit`           | past `maxFileSize`; a write that would leave the disk less than the store keeps free | write less, or free the disk         |
| `corrupt`         | a whole read whose bytes do not match their hash                                     | restore the file from a backup       |
| `closed`          | the store closed                                                                     | open it again                        |
| `outcome unknown` | the commit failed after the write ran                                                | `head` the path before writing again |

Every error names the files and the path: `files avatars: "users/42/1.png": conflict`.
`get` and `head` of a path with no file, and `delete` of one, are not errors.

## Bounds

| What                      | Bound                               |
|---------------------------|-------------------------------------|
| a path with its folders   | 1 KiB and 16 segments               |
| a file                    | none, unless `maxFileSize` sets one |
| free disk the store keeps | 1 GiB, `keepFree` on the store      |
| a content type            | 256 bytes                           |
| `meta`                    | 2 KiB in all                        |
| a page                    | 1000 files                          |
| a name                    | `[a-z0-9][a-z0-9_-]{0,63}`          |

## What the engine chooses

None of this appears in a call, and each is measured in the Go engine's rounds:

- a file up to 16 KiB is a row of `blobs/blobs.db`, sharing its commit with
  the writes beside it; a larger one is a file of its own under
  `blobs/objects/`, synced with its directory;
- the hash is SHA-256, the scrub's pace a slice a minute;
- a file's bytes are never changed once stored: a replace writes new ones,
  and those no file names any more are removed after the commit that dropped
  them.

A store's backup, a snapshot of every engine at one moment, is the store's,
in phase 4, and covers files with the rest.

## Later: storing what repeats once

Not in the first version, and not designed yet. Backups are the case: a
night's dump of a database is mostly yesterday's, and storing it whole every
night stores the same bytes again. Cut by their content, as restic, borg and
kopia cut theirs, two versions of a file share the pieces that did not
change, and only the pieces that did are stored.

What it would cost: an index of pieces, thousands of rows for a large file;
pieces counted by the files that name them and removed with the last; a read
that puts a file together from its pieces; and the cutting, at a gigabyte or
two a second. Checking a range would come with it, but not free: the pieces a
range touches are hashed and compared.

- It is a choice of the files, made where they open, never of a call. Its
  word is open: the mechanism is deduplication by content, which is not
  `incremental`, a word for a series of changes.
- Before it is built, a round measures it on real dumps: what a night's dump
  shares with the night before, at what speed, when a dump's rows move.
- The first version keeps the files' places so that a stored file can later be
  a list of pieces, without a call changing.

## Newcomer check, 10 October

Three Haiku agents with no other context: one said what each of 28 call sites
does, one compared spellings blind, one reviewed the API as a developer
choosing a library for uploads, exports and backups.

| They found                                                                                  | Change                                                                                              |
|---------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------|
| `under('users', 42)` the hardest line: is it the path `users/42`, does it hold `users/420`? | `folder('users', 42)`: here a folder's segments and its files' paths are one path; kv keeps `under` |
| `const exports = …` is a SyntaxError in a CommonJS module                                   | the examples say `reports`                                                                          |
| `maxSize` read as a quota over the set                                                      | `maxFileSize`, the owner's reviewer read it the same way                                            |
| `'2GB'`: 10⁹ bytes or 2³⁰?                                                                  | `KiB`, `MiB`, `GiB`, `TiB` or bytes; `GB` is `invalid`                                              |
| `undefined` from `create` meant a file was there, from `get` that none was                  | `create` returns a boolean, as kv's does                                                            |
| `upload.create()` read as starting another upload                                           | gone: `create` takes a stream as `put` does                                                         |
| `abort()` after `commit()`: does it undo the commit?                                        | `await using`, as Python's `async with`: leaving the block without `commit` aborts                  |
| `copy` and `rename` onto a file replace it silently, as `fs.rename` and S3's copy do        | a conflict, unless `ifMatch` names the version to replace                                           |
| `{ files, bytes }` from `usage()` beside the method `bytes()`                               | `{ count, size }`                                                                                   |
| `modified` is neither HTTP's `Last-Modified` nor the web's `lastModified`                   | `lastModified`                                                                                      |
| what the ETag is: no way to check a copy kept elsewhere                                     | the quoted hex SHA-256 of the bytes                                                                 |
| the files' `ttl` read as maybe a ceiling                                                    | the book says it is the default term                                                                |
| an empty id in a folder could name `users/` and clear every user                            | an empty segment is `invalid`                                                                       |
| examples that do not compile in strict TypeScript, `doc.etag` of an `undefined`             | the examples check                                                                                  |
| serving a path taken from the request, and a type a user gave from the program's origin     | the serving example reads the signed-in user's file, and the book says how to serve uploads         |
| whether a `put` that returned survives a power loss                                         | the book says, by durability                                                                        |
| `bytes()` of a file of 2 GiB                                                                | the book says it holds the whole file; `stream()` for large ones                                    |

Kept as they are: `rename`, which one reader would have called `move`, since
`move` is a keyword in Rust and `rename` is Node's and POSIX's; `create`
answering with a boolean rather than a `conflict`, which one reader asked for,
as kv's `create` answers since its own check; `put` and an upload replacing
what a path held, said in `put`'s first line; `head`, which a reader of the
shell may take for the first lines of a file, since HTTP and S3 name a read
without the body so; absence as `undefined`, as kv's.

Not in the first version, which the review asked about: a quota over a
folder, resuming an upload, verifying a store's files against a list kept
elsewhere, and a store's own backup, phase 4's.

## Was, in Go

| Was                                          | Now                                        |
|----------------------------------------------|--------------------------------------------|
| `blobs.Open(ctx, store, opts)`, `OpenBucket` | `store.files(…)`; the engine opens with it |
| `Of(owners...)`                              | `folder`                                   |
| `Open(ctx, key)` → `*Reader`                 | `get` → `StoredFile`                       |
| `Stat`                                       | `head`                                     |
| `Create` → `*Upload`                         | `upload`                                   |
| `Move`                                       | `rename`; `move` is a keyword in Rust      |
| `Scan`, `All`                                | `list`, `all`                              |
| `IfNoneMatch("*")`                           | `create`, as kv's                          |
| `DefaultTTL(d)`, `ExpireAt(t)`               | `ttl` on the files and a write; `expire`   |
| `MaxSize`                                    | `maxFileSize`; `maxSize` read as a quota   |
| `Object`                                     | `FileInfo`                                 |
| `Usage{Objects, Bytes}`                      | `{ count, size }`                          |
| `Modified`                                   | `lastModified`                             |
