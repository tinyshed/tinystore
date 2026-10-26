# Serving files

A file carries what an HTTP response needs: its type, its size, a strong ETag
and when it was written. The engine knows no HTTP and checks no one's right
to a file; your handler decides which path a request may read, and sends the
bytes.

```ts
Bun.serve({
	async fetch(request) {
		const user = await signedIn(request) // yours
		const file = await avatars.get(`${user.id}.png`)
		if (!file) return new Response('not found', { status: 404 })
		if (request.headers.get('if-none-match') === file.etag) return new Response(null, { status: 304 })
		return new Response(file.stream(), {
			headers: { 'content-type': file.contentType, etag: file.etag, 'content-length': String(file.size) },
		})
	},
})
```

```python
@app.get("/avatar")
async def avatar(request: Request) -> Response:
    user = await signed_in(request)  # yours
    file = await avatars.get(f"{user.id}.png")
    if file is None:
        return Response("not found", status_code=404)
    if request.headers.get("if-none-match") == file.etag:
        return Response(status_code=304)
    headers = {"etag": file.etag, "content-length": str(file.size)}
    return StreamingResponse(file, media_type=file.content_type, headers=headers)  # Starlette's
```

```go
file, found, err := avatars.Get(ctx, fmt.Sprintf("%d.png", user.ID))
if err != nil || !found {
	http.NotFound(w, r)
	return
}
defer file.Close()
w.Header().Set("Content-Type", file.ContentType)
w.Header().Set("ETag", file.ETag)
http.ServeContent(w, r, "", file.LastModified, file) // Range, If-None-Match and If-Modified-Since
```

```rust
let Some(mut file) = avatars.get(&format!("{}.png", user.id))? else {
    return Ok(not_found());
};
let info = file.info().clone();
let body = file.read_all()?; // checked against its SHA-256
Ok(response(200, &info.content_type, &info.etag, body))
```

The ETag is the quoted SHA-256 of the bytes, `"2cf24dba…"`: it depends on
the bytes alone, so two files with the same bytes share one, and a cache that
holds it holds the right version.

<div data-for="go">

In Go a `StoredFile` is an `io.ReadSeekCloser`, so `http.ServeContent`
answers ranges, `If-None-Match` and `If-Modified-Since` itself.

</div>

## A file stays what it was at `get`

In the process that holds the store, `get` holds the file open: the bytes it
reads stay what they were when `get` returned, through a replace, a delete or
an expiry, on Windows too (contract BLOB-03). A download that started before
a new avatar was uploaded finishes with the old one.

Over a connection to a server, `get` brings the file's bytes when they fit
one message, about a megabyte. A larger file's bytes are read when they are
read, on the ETag `get` gave: if it was replaced or deleted since, the read is
`conflict`, never the other version's bytes.

## Ranges

A video player and a resumed download ask for a range:

```ts
const range = parseRange(request.headers.get('range'), file.size) // yours: { start, end }
return new Response(file.slice(range.start, range.end).stream(), {
	status: 206,
	headers: { 'content-range': `bytes ${range.start}-${range.end - 1}/${file.size}`, etag: file.etag },
})
```

```python
start, end = parse_range(request.headers["range"], file.size)  # yours
return StreamingResponse(file.slice(start, end), status_code=206, media_type=file.content_type,
                         headers={"content-range": f"bytes {start}-{end - 1}/{file.size}", "etag": file.etag})
```

```go
http.ServeContent(w, r, "", file.LastModified, file) // reads the Range header itself
```

```rust
let mut chunk = vec![0; (end - start) as usize];
let read = file.read_at(&mut chunk, start)?;
```

`slice(-500)` is the last 500 bytes, as `Blob.slice` and Python's slices
read. A range is not checked against the hash: only a whole read is.

## Serve uploads safely

A content type a user gave is the user's. An upload served as `text/html` or
`image/svg+xml` from your own origin runs its scripts there, with your
cookies. Serve what users uploaded one of three ways:

| Way                                                    | Effect                                         |
|--------------------------------------------------------|------------------------------------------------|
| `content-disposition: attachment; filename="…"`        | the browser downloads it instead of opening it |
| another origin, `files.example-cdn.com`                | its scripts run where your cookies are not     |
| a type you chose: `image/png` after checking the bytes | the browser never reads it as a page           |

The name a user uploaded the file as is a good `meta` field, and
`filename*=UTF-8''…` sends one that is not ASCII.

## Caching

The ETag changes when the bytes do, so a URL that names the version can be
cached for good:

```ts
const info = await avatars.head(`${user.id}.png`) // what it carries, none of its bytes
const url = `/avatars/${user.id}.png?v=${info!.etag.slice(1, 13)}` // cache-control: public, max-age=31536000, immutable
```

```python
info = await avatars.head(f"{user.id}.png")
url = f"/avatars/{user.id}.png?v={info.etag[1:13]}"
```

```go
info, found, err := avatars.Head(ctx, fmt.Sprintf("%d.png", user.ID))
url := fmt.Sprintf("/avatars/%d.png?v=%s", user.ID, info.ETag[1:13])
```

```rust
let info = avatars.head(&format!("{}.png", user.id))?.expect("an avatar");
let url = format!("/avatars/{}.png?v={}", user.id, &info.etag[1..13]);
```

`head` reads what a file carries and none of its bytes.

## See also

- [Blobs](README.md): the engine's overview.
- [Integrity](integrity.md): what a whole read checks.
- [Attachments](../../handbook/chat/attachments.md) in the Handbook serves a
  chat's uploads.
