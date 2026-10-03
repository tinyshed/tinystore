# Blobs

The blobs engine stores your application's files: avatars, chat attachments,
camera recordings, exports, documents. A file is stored by a path, appears
completely or not at all, and is checked against its hash when you read it in
full. Small files live in `data/blobs/blobs.db`, and larger ones as one file
each under `data/blobs/objects/`.

## Store and read a file

```ts
const files = store.blobs.bucket('files')

await files.put('avatars/42.png', Bun.file('avatar.png'), { contentType: 'image/png' })

const avatar = await files.get('avatars/42.png') // undefined if there is no such file
const bytes = await avatar?.bytes()
```

```python
files = store.blobs.bucket("files")

await files.put("avatars/42.png", Path("avatar.png").read_bytes(), content_type="image/png")

avatar = await files.get("avatars/42.png")  # None if there is no such file
data = await avatar.read()
```

```go
objects, err := blobs.Open(ctx, store, blobs.Options{}) // data/blobs/
files, err := blobs.OpenBucket(ctx, objects, "files")

f, err := os.Open("avatar.png")
obj, err := files.Put(ctx, "avatars/42.png", f, blobs.ContentType("image/png"))

avatar, found, err := files.Open(ctx, "avatars/42.png") // found is false if there is no such file
defer avatar.Close()
data, err := io.ReadAll(avatar)
```

`put` returns after the file is saved to disk. Whatever the path held before
is replaced at that moment. Nobody ever sees half of a file, neither while it
is being written nor after a crash.

In Go, `Open` returns a reader with `Read`, `ReadAt`, `Seek` and `Close`. In Bun,
`get` returns the object with its `body` as a stream, and `bytes()`, `text()`
and `json()`. In Python, `get` returns an object that you read with `read()`
or iterate over in chunks.

## Paths and folders

```ts
const media = store.blobs.bucket('media')

await media.of('users', 42).put('photos/1.jpg', photo) // stored as users/42/photos/1.jpg
await media.put('users/42/photos/1.jpg', photo)        // the same file

const page = await media.of('users', 42).scan({ prefix: 'photos/' })
const { objects, bytes } = await media.of('users', 42).usage()
await media.of('users', 42).clear() // deletes every file of user 42
```

```python
media = store.blobs.bucket("media")

await media.of("users", 42).put("photos/1.jpg", photo)  # stored as users/42/photos/1.jpg
await media.put("users/42/photos/1.jpg", photo)         # the same file

page = await media.of("users", 42).scan(prefix="photos/")
usage = await media.of("users", 42).usage()  # usage.objects, usage.bytes
await media.of("users", 42).clear()          # deletes every file of user 42
```

```go
media, err := blobs.OpenBucket(ctx, objects, "media")

obj, err := media.Of("users", 42).Put(ctx, "photos/1.jpg", photo) // stored as users/42/photos/1.jpg
obj, err = media.Put(ctx, "users/42/photos/1.jpg", photo)         // the same file

page, err := media.Of("users", 42).Scan(ctx, blobs.Query{Prefix: "photos/"})
usage, err := media.Of("users", 42).Usage(ctx)
err = media.Of("users", 42).Clear(ctx) // deletes every file of user 42
```

A key is a path of segments separated by `/`. `of` names a folder. Folders
don't need to be created or deleted: a folder exists while it contains files.
`of('users', 4)` never contains the files of user 42, even though the text
`users/4` is a prefix of `users/42`.

- `scan` lists every file under a folder, including subfolders, a page at a
  time, sorted by path. `all` walks every page for you.
- `usage` counts the files and bytes under a folder. It is a check, not a
  limit: two uploads that each pass a check can exceed a quota together.
- `clear` deletes a folder and everything under it, immediately, however many
  files it has.

A key names nothing on disk, because files are stored under numbers. So
`Photo.jpg` and `photo.jpg` are two different keys on every operating system,
and a name that Windows reserves, such as `CON`, is a valid key.

## What a file carries

| Field | Meaning |
|---|---|
| `key` | its path, relative to the folder you asked |
| `size` | its size in bytes |
| `etag` | a quoted hash of its bytes, ready for an HTTP `ETag` header |
| content type | the type you gave when you stored it |
| `modified` | when this version was saved |
| `expires` | when it expires, if it does |
| `meta` | a few strings of your own, such as the original file name |

The ETag depends only on the bytes: two files with the same bytes have the
same ETag. Store everything else about a file, such as its owner or album, in
your database, and keep the file's key there.

## In this section

- [Uploads](uploads.md): stream large files, limit their size, and what a
  crash leaves behind.
- [Serving files](serving.md): HTTP range requests, caching and conditional
  writes.
- [Copies, moves and expiry](copies.md): files that expire, and copies that
  share their bytes.
- [Integrity](integrity.md): how TinyStore detects a changed byte.

## Limits and defaults

| | |
|---|---|
| A path, including its folder | 1 KiB and 16 segments |
| A file | no limit, unless the bucket sets one |
| Stored inside the database | files up to 16 KiB |
| A content type | 256 bytes |
| Meta | 2 KiB in total |
| A `scan` page | 1,000 files |
| A bucket name | `[a-z0-9][a-z0-9_-]{0,63}` |

## See also

- [blobs/README.md](../../blobs/README.md): the full contract of the blobs
  engine.
- [design/blobs.md](https://github.com/tinyshed/research/blob/main/tinystore/design/blobs.md)
  in the research repository: why blobs work this way, with measurements.
