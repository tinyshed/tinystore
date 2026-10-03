# Copies, moves and expiry

Copy and move files without copying their bytes, and let files delete
themselves after a time. Together, these cover uploads that wait for a message
to be sent, exports that live for a day, and recordings that are kept for a
month.

## Files that expire

```ts
const exports = store.blobs.bucket('exports', { defaultTtl: '24h' })

await exports.put(`${userId}/notes.zip`, archive)               // gone in 24 hours
await exports.put(`${userId}/report.pdf`, pdf, { ttl: '1h' })    // gone in 1 hour
```

```python
exports = store.blobs.bucket("exports", default_ttl="24h")

await exports.put(f"{user_id}/notes.zip", archive)            # gone in 24 hours
await exports.put(f"{user_id}/report.pdf", pdf, ttl="1h")     # gone in 1 hour
```

```go
exports, err := blobs.OpenBucket(ctx, objects, "exports", blobs.DefaultTTL(24*time.Hour))

obj, err := exports.Of(userID).Put(ctx, "notes.zip", archive)                   // gone in 24 hours
obj, err = exports.Of(userID).Put(ctx, "report.pdf", pdf, blobs.TTL(time.Hour)) // gone in 1 hour
```

Every write gives the file a new expiry: the write's own `ttl` or `expireAt`,
or the bucket's default TTL, or none. An export that you generate again lives
for another full day.

An expired file is gone for every call: `get` finds nothing and `scan`
doesn't list it. TinyStore removes expired files and their bytes in the
background, up to 10,000 per transaction every minute.

## Move a file

A chat composer uploads attachments while the user types. Keep them under
`pending/` for a day, and move them when the message is sent:

```ts
const files = store.blobs.bucket('chat-files')
const mine = files.of('users', userId)

await mine.put(`pending/${uploadId}`, body, { ttl: '24h' })

// when the message is sent
await mine.move(`pending/${uploadId}`, `sent/${uploadId}`)
```

```python
files = store.blobs.bucket("chat-files")
mine = files.of("users", user_id)

await mine.put(f"pending/{upload_id}", body, ttl="24h")

# when the message is sent
await mine.move(f"pending/{upload_id}", f"sent/{upload_id}")
```

```go
files, err := blobs.OpenBucket(ctx, objects, "chat-files")
mine := files.Of("users", userID)

obj, err := mine.Put(ctx, "pending/"+uploadID, body, blobs.TTL(24*time.Hour))

// when the message is sent
obj, err = mine.Move(ctx, "pending/"+uploadID, "sent/"+uploadID)
```

A move is one write. It doesn't copy any bytes, and the moved file gets the
expiry that a new `put` would give it, here none. An attachment that is never
sent expires after a day, and you don't need a cleanup job.

Move the files before you insert the message row. If the program crashes in
between, you are left with a file that no message points to, instead of a
message whose file is missing.

## Copy a file

```ts
await media.copy('templates/welcome.png', `users/${userId}/banner.png`)
```

```python
await media.copy("templates/welcome.png", f"users/{user_id}/banner.png")
```

```go
obj, err := media.Copy(ctx, "templates/welcome.png", fmt.Sprintf("users/%d/banner.png", userID))
```

A copy shares the bytes of its source. Copying a 4 GB file takes the same time
as copying a 4 KB one, and uses no extra disk space. The bytes stay until the
last file that uses them is deleted. A copy keeps the source's content type
and meta, unless you give others in the call. It gets the expiry that a new
`put` would give it.

Copies and moves stay within one bucket. Copying or moving a key that doesn't
exist fails with a conflict error. Both accept `ifNoneMatch` and `ifMatch` for
the destination.

## Delete

```ts
await files.delete('avatars/42.png')                    // deleting a missing file is not an error
await files.delete('avatars/42.png', { ifMatch: etag }) // only this version
await files.of('users', 42).clear()                     // the whole folder
```

```python
await files.delete("avatars/42.png")                  # deleting a missing file is not an error
await files.delete("avatars/42.png", if_match=etag)   # only this version
await files.of("users", 42).clear()                   # the whole folder
```

```go
err = files.Delete(ctx, "avatars/42.png")                     // deleting a missing file is not an error
err = files.Delete(ctx, "avatars/42.png", blobs.IfMatch(etag)) // only this version
err = files.Of("users", 42).Clear(ctx)                        // the whole folder
```

`clear` removes a folder and every folder under it. Up to 10,000 files are
deleted at once. A larger folder is hidden immediately and deleted in the
background. A file written to the folder after the `clear` is kept.

## See also

- [Blobs](README.md): paths, folders and usage.
- [blobs/README.md](../../blobs/README.md): the full contract of copies and
  expiry.
