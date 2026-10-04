# Uploads

Store files of any size as a stream, without holding them in memory. An upload
either completes, or leaves nothing behind: not after an error, not after a
cancelled request, not after a crash.

## Stream a request body

```ts
const files = store.blobs.bucket('files', { maxSize: 20 << 20 })

const obj = await files.put(`uploads/${id}`, request.body!, {
	contentType: request.headers.get('content-type') ?? undefined,
	size: Number(request.headers.get('content-length')),
})
```

```python
files = store.blobs.bucket("files", max_size=20 << 20)

obj = await files.put(
    f"uploads/{upload_id}",
    request.stream(),
    content_type=request.headers.get("content-type"),
    size=int(request.headers["content-length"]),
)
```

```go
files, err := blobs.OpenBucket(ctx, objects, "files", blobs.MaxSize(20<<20))

obj, err := files.Put(ctx, "uploads/"+id, r.Body,
	blobs.ContentType(r.Header.Get("Content-Type")), blobs.Size(r.ContentLength))
```

`put` reads the stream and writes it to a temporary file as the bytes arrive,
so memory use doesn't grow with the file's size. Only when the whole stream
has arrived and been synced to disk does the file appear under its key.

- **`size`** tells TinyStore how long the stream should be. A stream that ends
  early or runs past it fails with an invalid argument error and leaves
  nothing. In Go, a negative size means unknown, so you can always pass
  `r.ContentLength`.
- **`maxSize`** limits the files of a bucket. An upload that goes past it stops
  with a limit error (`ErrLimit`, `LimitError`) and leaves nothing. HTTP's
  `413 Content Too Large` is the matching answer.

## Write a file piece by piece (Go)

```go
upload, err := exports.Of(userID).Create(ctx, "notes.zip", blobs.ContentType("application/zip"))
if err != nil {
	return err
}
defer upload.Abort() // does nothing once the upload is committed

zw := zip.NewWriter(upload)
if err := writeNotes(zw, userID); err != nil {
	return err
}
if err := zw.Close(); err != nil {
	return err
}
export, err := upload.Commit(ctx)
```

`Create` returns an `io.Writer`, so any Go code that writes, such as a zip or
an image encoder, can write a file directly into the store. The file appears
at `Commit`. `Abort` throws the upload away, so `defer upload.Abort()` is
always safe.

In Bun and Python, pass a stream or an async iterable to `put` instead:

```ts
async function* minutes() {
	for await (const chunk of camera) yield chunk
}
await recordings.put(`cam-1/${minute}.mp4`, minutes(), { contentType: 'video/mp4' })
```

```python
async def chunks():
    async for chunk in camera:
        yield chunk


await recordings.put(f"cam-1/{minute}.mp4", chunks(), content_type="video/mp4")
```

## Uploads that don't finish

| What happens                             | What is left                                        |
|------------------------------------------|-----------------------------------------------------|
| the stream fails, or `abort` is called   | nothing                                             |
| the request is cancelled                 | nothing: the upload ends with its context or signal |
| the process crashes during the upload    | nothing: the next `open` removes the partial file   |
| the process crashes after `put` returned | the complete file                                   |

TinyStore writes the bytes to a temporary file, syncs it, moves it into place,
and only then commits the database row that names it. A row never points to a
file that isn't complete. The next time the store opens, it removes whatever
an interrupted upload left behind.

## Keep the disk from filling up

An upload stops with a limit error if the disk would have less than 1 GiB free
after it. This protects the other engines: a single large upload can't take
the space that the database files need to commit. In Go, change the reserve
with `blobs.Options{KeepFree: …}`. A sidecar that Bun or Python starts keeps
the default.

TinyStore checks the space when an upload with a known size starts, and every
64 MiB during the upload.

## Upload only once

```ts
await files.put(key, body, { ifNoneMatch: true }) // ConflictError if the key already exists
```

```python
await files.put(key, body, if_none_match=True)  # ConflictError if the key already exists
```

```go
obj, err := files.Put(ctx, key, body, blobs.IfNoneMatch()) // ErrConflict if the key already exists
```

A client that retries an upload after a timeout can't create a second copy.
The condition is checked when the upload starts, so a doomed upload of
gigabytes stops before its first byte, and again when it commits. See
[Serving files](serving.md) for conditions on replacing a file.

## Limits and defaults

|                          |                                                |
|--------------------------|------------------------------------------------|
| A file                   | no limit, unless the bucket sets `maxSize`     |
| Free disk space kept     | 1 GiB                                          |
| Memory per upload        | 16 KiB, up to 80 KiB briefly for a long upload |
| Uploads at the same time | 1,024                                          |

## See also

- [Serving files](serving.md): send a file over HTTP with ranges and caching.
- [blobs/README.md](../../blobs/README.md): the full contract of uploads.
