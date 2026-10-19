# Attachments

Users attach files to their messages: a PDF to summarise, a photo to describe.
This chapter stores each upload as it streams in, serves it back with the
right headers, and deletes a chat's files with the chat. It uses the Blobs
engine, which keeps files by path in the store's `blobs/` directory.

## Open the files

```ts
const attachments = store.files('attachments', { maxFileSize: '20MiB' })
```

```python
attachments = store.files("attachments", max_file_size="20MiB")
```

```go
attachments, err := blobs.OpenFiles(store, "attachments", blobs.MaxFileSize(20<<20))
```

```rust
let attachments = store.files("attachments").max_file_size(20 << 20).open()?;
```

`attachments` is a named set of files by path. A file larger than 20 MiB is
refused and leaves nothing behind. Without `maxFileSize` there is no limit,
because a store may keep videos of many gigabytes.

The chat keeps a chat's files in a folder of their own, and names each file
by its message and its number in the message:

```text
chats/7/41/1     the first attachment of message 41 in chat 7
chats/7/41/2     the second
```

The user's own file name is not part of the path. It goes into the file's
`meta`, a few strings the file carries. A name such as `../../etc/passwd` or
`report/2026.pdf` can then never change where a file goes.

## Store an upload

The request's body is a stream. `put` writes it to the disk as it arrives,
without holding the file in memory:

```ts
async function attach(request: Request, chatId: number, messageId: number, n: number) {
	const name = request.headers.get('x-file-name') ?? 'file'
	return attachments.folder('chats', chatId).put(`${messageId}/${n}`, request.body!, {
		contentType: request.headers.get('content-type') ?? 'application/octet-stream',
		size: Number(request.headers.get('content-length')),
		meta: { name },
	})
}
```

```python
async def attach(request: Request, chat_id: int, message_id: int, n: int) -> tinystore.FileInfo:
    return await attachments.folder("chats", chat_id).put(
        f"{message_id}/{n}",
        request.stream(),  # Starlette's: an async iterator of bytes
        content_type=request.headers.get("content-type", "application/octet-stream"),
        size=int(request.headers["content-length"]),
        meta={"name": request.headers.get("x-file-name", "file")},
    )
```

```go
func attach(r *http.Request, chatID, messageID int64, n int) (blobs.FileInfo, error) {
	return attachments.Folder("chats", chatID).Put(r.Context(), fmt.Sprintf("%d/%d", messageID, n), r.Body,
		blobs.ContentType(r.Header.Get("Content-Type")),
		blobs.Size(r.ContentLength),
		blobs.Meta(map[string]string{"name": r.Header.Get("X-File-Name")}))
}
```

```rust
fn attach(body: &mut impl Read, upload: &Incoming, chat_id: i64, message_id: i64, n: u32) -> Result<FileInfo, AppError> {
    let mut writing = attachments
        .folder("chats")
        .folder(chat_id)
        .key(&format!("{message_id}/{n}"))
        .content_type(&upload.content_type)
        .size(upload.content_length)
        .meta("name", &upload.file_name)
        .upload()?;
    std::io::copy(body, &mut writing)?;
    Ok(writing.commit()?) // dropped without commit, the upload leaves nothing
}
```

`put` returns the file's info: its `size`, its `etag`, its `contentType` and
its `meta`. `size` is the length the client declared: a body that turns out
longer or shorter is refused. In Rust, an `upload` takes the bytes from any
reader, and `commit` publishes the file.

## An upload cut halfway

A phone on a train starts uploading a 100 KB photo, and the connection drops
at 60 KB:

```scene
code ts
  return attachments.folder('chats', chatId).put(`${messageId}/${n}`, request.body!, {
    contentType: request.headers.get('content-type') ?? 'application/octet-stream',
    size: Number(request.headers.get('content-length')),
  })
end
actor phone "Phone"        at 8,50  icon=smartphone
actor app   "Your app"     at 40,50 icon=server
actor blobs "blobs/objects" at 72,30 icon=folder
actor kv    "blobs.db"      at 72,78 icon=database
link phone app
link app blobs
link app kv
set kv "41/1: plan v1"

step "The phone uploads a new version of 41/1. put streams its bytes to a place of their own in blobs/, and counts their hash on the way." line=1
  send phone -> app "20 KB"
  send app -> blobs "20 KB"
  send phone -> app "40 KB"
  send app -> blobs "40 KB"
  set blobs "60 KB, not published"

step "The connection drops at 60 KB. The stream ends with an error, so put fails and publishes nothing." line=1-4
  cut phone app
  send phone -> app "…" lost
  say app "put failed"

step "The half-written bytes are removed. Every reader still gets the old file, whole, as it was." line=1
  strike blobs "60 KB, not published"
  send kv -> app "41/1: plan v1" color=ok
```

A file appears whole at its commit, or not at all (contract BLOB-01). Nobody
ever reads half a file, while it is written or after a crash. If the process
itself dies in the middle of an upload, the next open of the store removes
what it left (contract BLOB-06).

## Serve a file

```ts
async function download(request: Request, chatId: number, path: string) {
	const file = await attachments.folder('chats', chatId).get(path)
	if (file === undefined) return new Response('not found', { status: 404 })
	if (request.headers.get('if-none-match') === file.etag) return new Response(null, { status: 304 })
	return new Response(file.stream(), {
		headers: {
			'content-type': file.contentType,
			'content-length': String(file.size),
			'content-disposition': `attachment; filename*=UTF-8''${encodeURIComponent(file.meta.name ?? 'file')}`,
			etag: file.etag,
		},
	})
}
```

```python
async def download(request: Request, chat_id: int, path: str) -> Response:
    file = await attachments.folder("chats", chat_id).get(path)
    if file is None:
        return Response("not found", status_code=404)
    if request.headers.get("if-none-match") == file.etag:
        return Response(status_code=304)
    name = quote(file.meta.get("name", "file"))
    headers = {
        "content-length": str(file.size),
        "content-disposition": f"attachment; filename*=UTF-8''{name}",
        "etag": file.etag,
    }
    return StreamingResponse(file, media_type=file.content_type, headers=headers)  # Starlette's
```

```go
func download(w http.ResponseWriter, r *http.Request, chatID int64, path string) {
	file, found, err := attachments.Folder("chats", chatID).Get(r.Context(), path)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	if !found {
		http.NotFound(w, r)
		return
	}
	defer file.Close()
	w.Header().Set("Content-Type", file.ContentType)
	w.Header().Set("Content-Disposition", mime.FormatMediaType("attachment", map[string]string{"filename": file.Meta["name"]}))
	w.Header().Set("ETag", file.ETag)
	http.ServeContent(w, r, "", file.LastModified, file) // Range, If-None-Match, If-Modified-Since
}
```

```rust
fn download(chat_id: i64, path: &str) -> Result<Option<(FileInfo, StoredFile)>, AppError> {
    let Some(file) = attachments.folder("chats").folder(chat_id).get(path)? else {
        return Ok(None); // 404
    };
    // StoredFile is Read + Seek: stream it, or seek for a range
    Ok(Some((file.info().clone(), file)))
}
```

The ETag is the SHA-256 of the file's bytes, already quoted as HTTP wants
it. A browser that has the file sends it back in `If-None-Match`, and the
server answers `304` without sending a byte. Go's `http.ServeContent` also
answers range requests, so a video seeks without downloading all of it. In
TypeScript a range is `file.slice(start, end)`.

> [!WARNING]
> **Serve uploads as attachments**
> A content type comes from the user. A file uploaded as `text/html` or
> `image/svg+xml` and served inline from your own domain runs its scripts
> there, with your users' cookies. Send `content-disposition: attachment`,
> serve uploads from another domain, or set the content type yourself.

## Delete a chat's files

```ts
await attachments.folder('chats', chatId).clear()
```

```python
await attachments.folder("chats", chat_id).clear()
```

```go
err = attachments.Folder("chats", chatID).Clear(ctx)
```

```rust
attachments.folder("chats").folder(chat_id).clear()?;
```

`clear` removes the folder and everything in it, for every reader at once,
however many files it holds. A folder is whole path segments, never a prefix
of text: `folder('chats', 7)` never touches chat 70's files. An empty or
missing id is refused, so a bug can never turn this call into "delete every
chat".

To see how much a user stores, `usage()` on a folder returns the number of
files and their bytes.

## Files that expire

A free plan keeps attachments for 30 days:

```ts
await attachments.folder('chats', chatId).put(path, request.body!, { ttl: '30d' })
```

```python
await attachments.folder("chats", chat_id).put(path, request.stream(), ttl="30d")
```

```go
_, err = attachments.Folder("chats", chatID).Put(ctx, path, r.Body, blobs.TTL(30*24*time.Hour))
```

```rust
attachments.folder("chats").folder(chat_id).key(&path).ttl(Duration::from_hours(24 * 30)).put(&bytes)?;
```

An expired file is gone for readers at once, and its bytes are removed in the
background. `files('attachments', { ttl: '30d' })` would set the same term for
every file written without its own.

## Why this design?

**Why not keep files in the SQL database.** SQLite can store blobs, but a 20
MB attachment in a row makes every backup, every copy of the database and
every page cache carry it. TinyStore keeps a file up to 16 KiB as a row of
`blobs/blobs.db`, where it shares a commit with other writes, and a larger one
as a file of its own under `blobs/objects/`. You never choose: the call is the
same.

**Why a path is not a file name on disk.** Files are stored under numbers,
and the path is only a key. So `Photo.jpg` and `photo.jpg` are two files on
Windows and macOS too, `CON` is a path like any other, and no path can climb
out of its folder (contract BLOB-08). Moving a store from Linux to Windows
changes nothing.

**Why a whole read is checked.** A disk can return bytes that changed since
they were written, and nothing in the file system says so. TinyStore hashes a
file as it reads it and compares the hash with the one taken at the write. It
holds the last bytes back until they match, so a damaged file fails with a
`corrupt` error before its end instead of reaching a user as good (contract
BLOB-02). A scrub in the background reads every file over 30 days and names
the ones that changed.

**Why bytes are never changed in place.** A replaced file writes new bytes
and switches the path to them in one commit. A reader that opened the old
version keeps reading it to the end, even while the file is replaced or
deleted (contract BLOB-03). This is also why a copy is free: `copy` adds a
path that names the same bytes, and the bytes go when the last path that
names them goes.

**Why there is no quota per folder.** Two uploads that each check a quota can
together pass it, unless every upload takes a lock. `usage` counts, and the
program decides what a plan allows. The store does keep one limit itself: a
write that would leave the disk with less than 1 GiB free is refused, so a
full disk never breaks the store's other files.

## Limits and defaults

| What                      | Limit                                                      |
|---------------------------|------------------------------------------------------------|
| a file                    | none, unless `maxFileSize` sets one                        |
| a path, with its folders  | 1 KiB and 16 segments                                      |
| a content type            | 256 bytes                                                  |
| `meta`                    | 2 KiB in all                                               |
| a page of `list`          | 1,000 files                                                |
| free disk the store keeps | 1 GiB, `keepFree` on the store                             |
| memory of an upload       | a few buffers of 64 KiB to 1 MiB, whatever the file's size |

## See also

- [Messages](messages.md): the database that names a message and its chat.
- [One directory, many engines](../start/directory.md#what-is-in-the-directory):
  where `blobs/` sits in the store.
- [Blobs API reference](../../reference/bun/blobs.md): every call of files,
  folders and uploads.
