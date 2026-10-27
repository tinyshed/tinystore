# Blobs

The Blobs engine keeps files by path in named sets, under `blobs/` of the
store's directory: avatars, attachments, exports. A file is written whole or
not at all, replaced whole, checked against its SHA-256 when it is read
whole, and served with an ETag. Folders group files by owner, so deleting a
user's files is one call.

```ts
const avatars = store.files('avatars', { maxFileSize: '5MiB' })

await avatars.folder('users', 42).put('avatar.png', request.body!, { contentType: 'image/png' })
const file = await avatars.folder('users', 42).get('avatar.png') // StoredFile | undefined
await avatars.folder('users', 42).clear() // every file of user 42
```

```python
avatars = store.files("avatars", max_file_size="5MiB")

await avatars.folder("users", 42).put("avatar.png", request.stream(), content_type="image/png")
file = await avatars.folder("users", 42).get("avatar.png")  # StoredFile | None
await avatars.folder("users", 42).clear()  # every file of user 42
```

```go
avatars, err := blobs.OpenFiles(store, "avatars", blobs.MaxFileSize(5<<20))

_, err = avatars.Folder("users", 42).Put(ctx, "avatar.png", r.Body, blobs.ContentType("image/png"))
file, found, err := avatars.Folder("users", 42).Get(ctx, "avatar.png") // io.ReadSeekCloser
err = avatars.Folder("users", 42).Clear(ctx)                          // every file of user 42
```

```rust
let avatars = store.files("avatars").max_file_size(5 << 20).open()?;

avatars.folder("users").folder(42).key("avatar.png").content_type("image/png").put(&png)?;
let file = avatars.folder("users").folder(42).get("avatar.png")?; // Option<StoredFile>
avatars.folder("users").folder(42).clear()?; // every file of user 42
```

## What it guarantees

- A file appears whole at its commit, or not at all: an upload that is cut,
  cancelled or of the wrong size leaves nothing (contract BLOB-01).
- A whole read is checked against the file's SHA-256 and fails `corrupt`
  before its end if a byte changed (contract BLOB-02).
- A reader keeps the version it opened through a replace or a delete
  (contract BLOB-03).
- Of two writes that read the same ETag, one conflicts, unless the first put
  back the very bytes it read: an ETag names the bytes alone (contract BLOB-04).
- A copy shares the bytes and outlives its source (contract BLOB-05).
- A path is its own bytes on every file system: `Photo.jpg` and `photo.jpg`
  are two files everywhere (contract BLOB-08).
- A set of files opened from a database, `db.files(…)`, commits with the
  database's rows in its transaction (contract DUR-06). See
  [Files in a database](../sql/buckets-and-queues.md#files-in-a-database).

## In these guides

| Page                                      | Covers                                                    |
|-------------------------------------------|-----------------------------------------------------------|
| [Uploads](uploads.md)                     | streams, `upload` and `commit`, `size`, `maxFileSize`     |
| [Serving files](serving.md)               | ETags, ranges, `content-disposition`, Go's `ServeContent` |
| [Copies and expiry](copies-and-expiry.md) | `copy`, `rename`, `ttl`, `expire`                         |
| [Integrity](integrity.md)                 | checked reads, the scrub, `ifMatch` and `create`          |

New to Blobs? [Attachments](../../handbook/chat/attachments.md) in the
Handbook stores and serves a chat's uploads.

## Async Rust

A Rust program on an async runtime, such as tokio, opens its files from
`tinystore::aio::Store`, with the `async` feature. The calls have the same
names as in the blocking API, and every call that reads, writes or waits
returns a future that you `.await`. This section shows only Rust, because in
Bun, Node and Python every call is awaited already, and in Go a call blocks
its goroutine, which costs little.

```rust
// blocking
let avatars = store.files("avatars").max_file_size(5 << 20).open()?;

let info = avatars.key("42.png").content_type("image/png").put(&png)?;
let file = avatars.get("42.png")?; // Option<StoredFile>
avatars.folder("users").folder(42).clear()?;
```

```rust
// async
use tinystore::Options;
use tinystore::aio::Store;

let store = Store::open("data", Options::default())?; // opening stays a plain call
let avatars = store.files("avatars").max_file_size(5 << 20).open()?;

let info = avatars.key("42.png").content_type("image/png").put(png).await?; // png: Vec<u8>, moved in
let file = avatars.get("42.png").await?; // Option<StoredFile>
avatars.folder("users").folder(42).clear().await?;
```

`put` and `create` take owned bytes, such as a `Vec<u8>`, a `String` or
`Bytes`, because a file larger than 16 KiB is written to disk on the store's
own threads, and the future may be gone by then. The futures of the files'
calls are `Send + 'static`, so you can pass them to `tokio::spawn`. Only an
upload's `write` and a file's `read_at` borrow their bytes.

The file that `get` returns is a `Stream` of its bytes, 256 KiB per piece.
Hand it to a response as it is:

```rust
let Some(file) = avatars.get("42.png").await? else { return Ok(not_found()) };
let body = axum::body::Body::from_stream(file); // checked against its SHA-256 as it streams
```

A stream that starts at the first byte is checked. If a byte changed on
disk, the last piece fails with a corrupt error (`ErrorKind::Corrupt`)
instead of arriving, so a client never gets the whole changed file (contract
BLOB-02). After `file.seek(...)` the stream is a range, and a range is not
checked. `read_all().await` reads the whole file, checked, and
`read_at(&mut buffer, offset).await` reads a range.

An upload takes each piece with `write`, and the file appears at `commit`:

```rust
let mut upload = avatars.key("43.png").content_type("image/png").upload().await?;
while let Some(chunk) = request.next().await { // request: a Stream of the body's chunks
    upload.write(&chunk?).await?;
}
let info = upload.commit().await?; // dropped without it, the upload aborts and leaves nothing
```

A list's page is awaited, and `all` is a `Stream` that reads one page at a
time:

```rust
let page = avatars.list().prefix("users/").page().await?;
let mut files = avatars.all();
while let Some(info) = files.next().await {
    println!("{}", info?.path);
}
```

| Blocking                                    | Async                                              |
|---------------------------------------------|----------------------------------------------------|
| `avatars.put(path, &bytes)?`                | `avatars.put(path, bytes).await?`, the bytes owned |
| `file.read_all()?`                          | `file.read_all().await?`                           |
| `std::io::copy(&mut file, &mut response)?`  | the file is a `Stream`: `Body::from_stream(file)`  |
| `std::io::copy(&mut request, &mut upload)?` | `upload.write(&chunk).await?` for each piece       |
| `for info in avatars.all()`                 | `while let Some(info) = files.next().await`        |

`tinystore::aio::blobs::Files::from(files)` and `files.blocking()` cross
between the two APIs. Both give the same set of files, so a command-line tool
can block on it while a server beside it awaits.

Maintenance has no async form. The store runs it by itself every minute.
Only a test that opens its store with `background: false` calls it, and that
call blocks: `tinystore::blobs::maintain(store.blocking())?`.

### Where the work runs

| Call                                          | Where it runs                                                                                    | If you drop its future after it was polled  |
|-----------------------------------------------|--------------------------------------------------------------------------------------------------|---------------------------------------------|
| `head`                                        | in the first poll when a reader is free, else on the store's threads                             | it answers in that poll, or stops           |
| `get`                                         | as `head`, but a file on disk is always opened on the store's threads                            | it answers in that poll, or stops           |
| a list's `page`, `all`, `usage`               | on the store's threads                                                                           | it stops                                    |
| a file's `read_all`, `read_at` and its stream | in the poll for a file kept inline, else on the store's threads                                  | it stops                                    |
| `delete`, `copy`, `rename`, `expire`, `clear` | queued in the first poll, committed on the store's threads                                       | it still commits                            |
| `put`, `create` up to 16 KiB                  | waits for the store's memory without a thread, then queued as `delete` is                        | it still commits, after its wait for memory |
| `put`, `create` past 16 KiB                   | waits for the store's memory without a thread, then written and committed on the store's threads | it still commits, after its wait for memory |
| `upload`                                      | waits for the store's memory without a thread                                                    | it leaves the line and holds nothing        |
| an upload's `write`                           | in the poll up to 16 KiB, then on the store's threads                                            | its piece is still written                  |
| an upload's `commit`                          | queued in the poll up to 16 KiB, else written and committed on the store's threads               | it still commits                            |

A small `put`, `create` or `commit` also runs on the store's threads when
TinyStore must first save a new block of ids for file contents. That happens
for the first file after the store opens, and then about once every 1,000
files.

No call commits, syncs the disk or waits on the thread that polls its future
(contract STORE-10). A file on disk is opened, read and written on the
store's threads, so your executor's threads never wait for the disk. A write
does nothing until its future is first polled. Once polled, it commits even
if you drop the future, and that includes a `put` that still waits for
memory (contract DUR-11). A read that you drop stops, and a whole read stops
between two pieces of 256 KiB (contract STORE-09).

[Rust with async](../../handbook/start/rust-async.md) in the Handbook covers
the async store, its transactions and its close.

## Reference

[Bun and Node](../../reference/bun/blobs.md) ·
[Python](../../reference/python/blobs.md) ·
[Go](../../reference/go/blobs.md) ·
[Rust](../../reference/rust/blobs.md)
