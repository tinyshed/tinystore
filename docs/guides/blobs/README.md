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
- Of two writes that read the same ETag, one conflicts (contract BLOB-04).
- A copy shares the bytes and outlives its source (contract BLOB-05).
- A path is its own bytes on every file system: `Photo.jpg` and `photo.jpg`
  are two files everywhere (contract BLOB-08).

## In these guides

| Page                                      | Covers                                                    |
|-------------------------------------------|-----------------------------------------------------------|
| [Uploads](uploads.md)                     | streams, `upload` and `commit`, `size`, `maxFileSize`     |
| [Serving files](serving.md)               | ETags, ranges, `content-disposition`, Go's `ServeContent` |
| [Copies and expiry](copies-and-expiry.md) | `copy`, `rename`, `ttl`, `expire`                         |
| [Integrity](integrity.md)                 | checked reads, the scrub, `ifMatch` and `create`          |

New to Blobs? [Attachments](../../handbook/chat/attachments.md) in the
Handbook stores and serves a chat's uploads.

## Reference

[Bun and Node](../../reference/bun/blobs.md) ·
[Python](../../reference/python/blobs.md) ·
[Go](../../reference/go/blobs.md) ·
[Rust](../../reference/rust/blobs.md)
