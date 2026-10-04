# Serving files

Send stored files over HTTP with caching, range requests for video players,
and conditional writes for editors. A blob carries everything HTTP needs: its
size, its type, its ETag and its modification time.

## Serve a file

```ts
Bun.serve({
	async fetch(req) {
		const key = new URL(req.url).pathname.slice(1)
		const head = await files.stat(key)
		if (!head) {
			return new Response('Not found', { status: 404 })
		}
		if (req.headers.get('if-none-match') === head.etag) {
			return new Response(null, { status: 304 }) // the browser's copy is current
		}
		const file = await files.get(key)
		return new Response(file!.body, {
			headers: {
				'Content-Type': file!.contentType ?? 'application/octet-stream',
				'Content-Length': `${file!.size}`,
				ETag: file!.etag,
			},
		})
	},
})
```

```python
@app.get("/files/{key:path}")
async def serve(key: str, request: Request):
    head = await files.stat(key)
    if head is None:
        raise HTTPException(404)
    if request.headers.get("if-none-match") == head.etag:
        return Response(status_code=304)  # the browser's copy is current
    download = await files.get(key)
    return StreamingResponse(download, media_type=head.content_type, headers={"ETag": head.etag})
```

```go
func (a *app) serveFile(w http.ResponseWriter, r *http.Request) {
	file, found, err := a.files.Open(r.Context(), r.PathValue("key"))
	switch {
	case err != nil:
		http.Error(w, "Unavailable", http.StatusServiceUnavailable)
		return
	case !found:
		http.NotFound(w, r)
		return
	}
	defer file.Close()

	w.Header().Set("ETag", file.ETag)
	w.Header().Set("Content-Type", file.ContentType)
	http.ServeContent(w, r, "", file.Modified, file) // ranges, If-None-Match, If-Range, HEAD
}
```

In Go, the reader from `Open` is an `io.ReadSeeker`, so the standard library's
`http.ServeContent` handles everything: ranges, several ranges at once,
`If-Range`, `If-None-Match`, `If-Modified-Since` and `HEAD`. The ETag is
already quoted, so you can set it as a header directly.

The file is streamed. Memory use doesn't depend on the file's size, so a
17 GB film is served the same way as an avatar.

## Serve a range

A video player that seeks asks for a range of bytes. Read only that range:

```ts
const minute = await recordings.get('cam-1/09-30.mp4', { offset: 5_000_000, length: 4 << 20 })
```

```python
minute = await recordings.get("cam-1/09-30.mp4", offset=5_000_000, length=4 << 20)
```

```go
video, found, err := recordings.Open(ctx, "cam-1/09-30.mp4")
defer video.Close()
part := io.NewSectionReader(video, 5_000_000, 4<<20) // or Seek, then Read
```

TinyStore reads the range directly from the file, and nothing before it. A
range is not checked against the file's hash, because checking requires
reading the whole file. See [Integrity](integrity.md).

## A reader keeps what it opened

Once you have opened a file, you can read it to the end, even if the file is
replaced, deleted, expires or the store closes in the meantime. A download
that started before an upload replaced the file finishes with the old bytes.
This works on Windows too, where a file that is open normally can't be
deleted.

## Write only if nothing changed

An editor sends back the ETag it read. The save succeeds only if nobody saved
a different version in between:

```ts
try {
	const doc = await documents.put(path, body, { ifMatch: req.headers.get('if-match')! })
	return new Response(null, { status: 204, headers: { ETag: doc.etag } })
} catch (err) {
	if (err instanceof ConflictError) return new Response(null, { status: 412 })
	if (err instanceof LimitError) return new Response(null, { status: 413 })
	throw err
}
```

```python
try:
    doc = await documents.put(path, body, if_match=request.headers["if-match"])
except ConflictError:
    return Response(status_code=412)
except LimitError:
    return Response(status_code=413)
return Response(status_code=204, headers={"ETag": doc.etag})
```

```go
doc, err := a.documents.Put(r.Context(), path, http.MaxBytesReader(w, r.Body, 1<<30),
	blobs.IfMatch(r.Header.Get("If-Match")), blobs.Size(r.ContentLength))
switch {
case errors.Is(err, tinystore.ErrConflict):
	w.WriteHeader(http.StatusPreconditionFailed)
case errors.Is(err, tinystore.ErrLimit):
	w.WriteHeader(http.StatusRequestEntityTooLarge)
case err != nil:
	http.Error(w, "Not saved", http.StatusInternalServerError)
default:
	w.Header().Set("ETag", doc.ETag)
	w.WriteHeader(http.StatusNoContent)
}
```

`ifMatch` accepts an `If-Match` header as it is: one ETag, a list of ETags, or
`*` for any existing file. `ifNoneMatch` creates a file only if the key is
empty, which makes a retried upload safe. Both conditions are checked before
the first byte is written, and again when the file is committed.

## HTTP and blobs

| HTTP                                                      | Blobs                                                         |
|-----------------------------------------------------------|---------------------------------------------------------------|
| `ETag`, `Content-Type`, `Content-Length`, `Last-Modified` | `etag`, the content type, `size`, `modified`                  |
| `Range`                                                   | `get` with an offset and a length; in Go, `http.ServeContent` |
| `If-None-Match: *` on `PUT`                               | `ifNoneMatch`                                                 |
| `If-Match` on `PUT` or `DELETE`                           | `ifMatch`                                                     |
| `412 Precondition Failed`                                 | a conflict error                                              |
| `413 Content Too Large`                                   | a limit error                                                 |

> [!WARNING]
> **Files uploaded by users**
> TinyStore stores a file's content type as it was given. A browser runs a
> `text/html` file as a page of your site. Serve uploaded files with
> `Content-Disposition: attachment`, or from a separate domain.

## See also

- [Uploads](uploads.md): streaming, size limits and crash safety.
- [blobs/README.md](../../blobs/README.md): the full contract of reads.
