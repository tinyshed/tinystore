# Blobs: the application's files

The design of the blobs engine, not built. Its questions were settled with
the user on 27 September after [the mechanics round](reports/blobs-mechanics-2026-09-27.md),
and the engine is built from this page with the user; what those decisions
were is under [Decided](#decided), and what is still to measure under
[Open](#open). Figures marked as the probe come from throwaway programs run on
27 September 2026 outside the repository; the round repeated them in
`spike/blobs_*`, and where the two differ the round's stand. The probe ran on an AMD Ryzen 7
7700 with a Samsung 990 PRO NVMe disk, on Windows 11 Pro 10.0.26200 with NTFS
and Defender's real-time protection on, and in Docker Desktop 29.6.2 on WSL2,
`golang:1.27` on a named ext4 volume; go1.27.1; random bytes from a seeded
ChaCha8, one run each: 500 objects a size for one writer, 2,048 for many
writers, and 512 in a second run of those.

## What it is for

Files a program keeps beside its data: avatars and their sizes, a chat's
attachments, a camera's recordings, an export a user downloads tomorrow, an
editor's documents. Today each is a directory the program writes by hand, with
a half-written file after a crash, two names that collide on a
case-insensitive disk, a delete that fails on Windows while someone downloads
the file and a backup that forgets it; or an S3 bucket beside a program that
has no other network dependency.

- **One process's files.** One store owns the directory, so blobs holds what
  one process writes. Another program reaches them through a server of its own
  that speaks [the protocol](#the-same-in-another-language).
- **Bytes, not documents.** A photo's owner, album and caption are rows of the
  application's database or values of kv, which name the photo by its key. An
  object carries what serving it needs, a type and a few strings, and nothing a
  query would search.
- **Not a file system.** An object is written whole and replaced whole: no
  partial writes, no appends, no directories to create or remove.
- **Its own engine.** `blobs/` holds `blobs.db` and the files; the package
  imports the root and `internal/`, never kv, sqldb or jobs, and never
  `net/http`: an application serves an object with the standard library's
  `http.ServeContent`.

## The public surface

```go
objects, err := blobs.Open(ctx, store, blobs.Options{}) // data/blobs/

avatars, err := blobs.OpenBucket(ctx, objects, "avatars")
exports, err := blobs.OpenBucket(ctx, objects, "exports", blobs.DefaultTTL(24*time.Hour))

obj, err := avatars.Of(user.ID).Put(ctx, "original", r.Body, blobs.ContentType("image/jpeg"))
photo, found, err := avatars.Of(user.ID).Open(ctx, "original") // Read, ReadAt, Seek, Close
err = avatars.Of(user.ID).Delete(ctx, "original")
```

- **A bucket is a name.** `OpenBucket` finds or creates it in `blobs.db`; its
  options are the program's and may change between runs. It holds bytes of any
  kind, so it has no type parameter.
- **The daily calls take a key.** `Put` stores what a reader yields and
  returns the object as stored; `Open` returns a reader of it and whether the
  key holds one, as kv's `Get` does.
- **Bytes a program produces over time are an upload.** `Create` returns a
  writer: bytes go in with `Write`, the object appears at `Commit`, and `Abort`
  leaves nothing, so `defer upload.Abort()` is always safe.
- **The engine's choices stay out of the calls.** Where bytes are kept, how
  they are checked and when they are synced appear in no call site.

```go
// Bucket
Put(ctx, key, r, opts...) (blobs.Object, error)      // ContentType, Meta, TTL, ExpireAt, Size, IfMatch, IfNoneMatch
Create(ctx, key, opts...) (*blobs.Upload, error)      // the same options; the object appears at Commit
Open(ctx, key) (*blobs.Reader, bool, error)           // io.ReadSeekCloser and io.ReaderAt, with the Object's fields
Stat(ctx, key) (blobs.Object, bool, error)
Delete(ctx, key, opts...) error                       // blobs.IfMatch; an absent key is not an error
Copy(ctx, from, to, opts...) (blobs.Object, error)    // shares the bytes; ContentType, Meta, TTL, ExpireAt, IfNoneMatch
Move(ctx, from, to, opts...) (blobs.Object, error)    // one write; the same options
Scan(ctx, blobs.Query) (blobs.Page, error)            // every key under this folder, in byte order
All(ctx, blobs.Query) iter.Seq2[blobs.Object, error]  // the same, a page at a time, no snapshot held
Usage(ctx) (blobs.Usage, error)                       // objects and bytes under this folder, counted
Clear(ctx) error                                      // this folder and every folder under it
Of(owners ...any) *blobs.Bucket

// Upload
Write(p) (int, error)
Commit(ctx) (blobs.Object, error)
Abort()                                               // after Commit it does nothing
```

## The same in another language

A server that serves `blobs/` to other programs is a module of its own and is
not built; the embedded API is shaped so that it can be. Every call is one
operation of the protocol, and what is not one, `All` and the reader's seeks,
is a loop over operations.

```text
put      bucket, key, bytes, type?, meta?, ttl?, size?, if?          → object | conflict
create   bucket, key, type?, meta?, ttl?, size?, if?                 → upload
write    upload, bytes                                               → written
commit   upload                                                      → object | conflict
abort    upload                                                      → ok
stat     bucket, key                                                 → object?
get      bucket, key, offset?, length?                               → object?, bytes
delete   bucket, key, if?                                            → ok | conflict
copy     bucket, from, to, type?, meta?, ttl?, if-none-match?        → object | conflict
move     bucket, from, to, type?, meta?, ttl?, if-none-match?        → object | conflict
scan     bucket, folder, prefix?, after?, limit                      → page
usage    bucket, folder                                              → objects, bytes
clear    bucket, folder                                              → ok
```

- **Nothing that cannot travel.** A key is text, a size, an offset and a
  length are integers, which a JavaScript number holds exactly up to 8 PiB, a
  time is unix milliseconds, an ETag its quoted text, meta a JSON object of
  strings, and bytes a stream: an HTTP body, or the frames of a connection.
- **An upload spans calls.** `create` answers with an id, each `write`
  appends, `commit` publishes; the server keeps the `Upload` by its id and ends
  it with the connection that made it. A `write` names no offset: an upload
  that loses its connection starts again, and resuming is
  [not in the first version](#not-in-the-first-version).
- **A range is a `get` with an offset and a length.** A player that seeks asks
  again from another offset.

```ts
const media = store.blobs.bucket("media")

const avatar = await media.put(`users/${id}/avatar.jpg`, file, { type: "image/jpeg" }) // a Blob, bytes or a stream
const { object, body } = await media.get(`users/${id}/avatar.jpg`)                    // body: a ReadableStream
const minute = await media.get("films/7.mkv", { offset: 5_000_000_000, length: 4 << 20 })

const upload = await media.create("recordings/cam-1/0930.mp4", { type: "video/mp4" })
for await (const chunk of camera) await upload.write(chunk)
const recording = await upload.commit()
```

HTTP maps onto the same model, and the standard library does the mapping:

| HTTP | blobs |
|---|---|
| `GET`, `HEAD` with `Range`, `If-Range`, `If-None-Match`, `If-Modified-Since` | `Open`, then `http.ServeContent` with the `ETag` header and `Modified` |
| `PUT` with a body | `Put(ctx, key, r.Body, blobs.Size(r.ContentLength))` |
| `If-None-Match: *` on `PUT` | `blobs.IfNoneMatch()` |
| `If-Match` on `PUT` or `DELETE` | `blobs.IfMatch(etag)` |
| `Content-Type`, `Content-Length`, `ETag`, `Last-Modified` | `ContentType`, `Size`, `ETag`, `Modified` |
| `412 Precondition Failed`, `413 Content Too Large` | `ErrConflict`, `ErrLimit` |

## Five cases

They are the API's examples, the gates' workloads and the round's.

**Avatars and their sizes.** The original and the sizes a page shows live in
the user's folder; a job makes the sizes from whatever original is there when
it runs, so two uploads before it ran are sized once.

```go
avatars, err := blobs.OpenBucket(ctx, objects, "avatars", blobs.MaxSize(20<<20))
sizing, err := jobs.OpenQueue[int64](ctx, queues, "avatar-sizes")

original, err := avatars.Of(user.ID).Put(ctx, "original", r.Body,
	blobs.ContentType(r.Header.Get("Content-Type")), blobs.Size(r.ContentLength))
err = sizing.Enqueue(ctx, user.ID, jobs.Key(fmt.Sprint("user:", user.ID)))

func (a *app) resize(ctx context.Context, job jobs.Job[int64]) error {
	mine := a.avatars.Of(job.Value)
	original, found, err := mine.Open(ctx, "original")
	if err != nil || !found {
		return err // deleted since: nothing to size
	}
	defer original.Close()
	img, _, err := image.Decode(original)
	if err != nil {
		return job.Fail(ctx, err) // not an image: no retry makes it one
	}
	for _, side := range []int{256, 64} {
		if err := a.saveSize(ctx, mine, side, scale(img, side)); err != nil {
			return err
		}
	}
	return nil
}

func (a *app) saveSize(ctx context.Context, mine *blobs.Bucket, side int, img image.Image) error {
	sized, err := mine.Create(ctx, fmt.Sprintf("%d.jpg", side), blobs.ContentType("image/jpeg"))
	if err != nil {
		return err
	}
	defer sized.Abort()
	if err := jpeg.Encode(sized, img, nil); err != nil {
		return err
	}
	_, err = sized.Commit(ctx)
	return err
}
```

A page asks for `/avatars/42/64.jpg` each time and gets `304 Not Modified`
while its ETag holds; deleting the account runs `avatars.Of(id).Clear(ctx)`.

**Chat attachments.** The composer uploads a file while its user types; a file
never sent is gone in a day, one sent moves out of `pending/` and stays, and
the user's folder answers a quota check by counting its objects' rows.

```go
files, err := blobs.OpenBucket(ctx, objects, "chat-files")

mine := files.Of("users", user.ID)
usage, err := mine.Usage(ctx)
if usage.Bytes+r.ContentLength > quota {
	http.Error(w, "your storage is full", http.StatusRequestEntityTooLarge)
	return
}
upload, err := mine.Put(ctx, "pending/"+uploadID, r.Body, blobs.TTL(24*time.Hour),
	blobs.Size(r.ContentLength), blobs.ContentType(r.Header.Get("Content-Type")),
	blobs.Meta("name", r.URL.Query().Get("name")))

for _, id := range draft.Uploads { // sending: a moved file takes the expiry a Put would give it, none here
	if _, err := mine.Move(ctx, "pending/"+id, "sent/"+id); err != nil {
		return err
	}
}
_, err = a.db.Exec(ctx, `insert into messages (id, chat, author, text, files) values (?, ?, ?, ?, ?)`,
	draft.ID, draft.Chat, user.ID, draft.Text, strings.Join(draft.Uploads, " "))

page, err := mine.Scan(ctx, blobs.Query{Prefix: "sent/", Limit: 50}) // "your files"
err = mine.Clear(ctx)                                                // the account deleted
```

The files move before the row is written, so a crash between them leaves a
file no message names rather than a message whose file is gone; see
[Two files, no transaction](#two-files-no-transaction).

**Recordings, and a player that seeks.** A camera writes one object a minute,
so a crash loses the minute being written and never the minutes before it; the
bucket keeps a month.

```go
recordings, err := blobs.OpenBucket(ctx, objects, "recordings", blobs.DefaultTTL(30*24*time.Hour))

func (a *app) record(ctx context.Context, camera *Camera) error {
	for minute := range camera.Minutes(ctx) {
		name := minute.Start.UTC().Format("2006-01-02/15-04") + ".mp4"
		segment, err := a.recordings.Of(camera.ID).Create(ctx, name, blobs.ContentType("video/mp4"))
		if err != nil {
			return err
		}
		if _, err = io.Copy(segment, minute); err == nil {
			_, err = segment.Commit(ctx)
		}
		segment.Abort()
		if err != nil {
			return err
		}
	}
	return ctx.Err()
}

func (a *app) play(w http.ResponseWriter, r *http.Request) {
	video, found, err := a.recordings.Of(r.PathValue("camera")).Open(r.Context(), r.PathValue("minute"))
	switch {
	case err != nil:
		http.Error(w, "unavailable", http.StatusServiceUnavailable)
		return
	case !found:
		http.NotFound(w, r)
		return
	}
	defer video.Close()
	w.Header().Set("ETag", video.ETag)
	w.Header().Set("Content-Type", video.ContentType)
	http.ServeContent(w, r, "", video.Modified, video) // Range, If-Range, If-None-Match, HEAD
}
```

A day is `Scan(ctx, blobs.Query{Prefix: "2026-09-27/"})`. A 17 GB film is
served the same way: a player that seeks to 1:37:20 asks for a range from
there, and the reader reads that range from the file and nothing before it.

**An export that expires.** A job streams a zip into an upload; the link works
for a day, and the bytes leave with the object.

```go
exports, err := blobs.OpenBucket(ctx, objects, "exports", blobs.DefaultTTL(24*time.Hour))

func (a *app) export(ctx context.Context, job jobs.Job[Export]) error {
	out, err := a.exports.Of(job.Value.User).Create(ctx, job.Value.ID+".zip",
		blobs.ContentType("application/zip"), blobs.Meta("name", "notes-"+job.Value.Day+".zip"))
	if err != nil {
		return err
	}
	defer out.Abort()
	if err := a.writeNotes(ctx, zip.NewWriter(out), job.Value.User); err != nil {
		return err
	}
	_, err = out.Commit(ctx)
	return err
}

// the download, from the reader Open returned, names the file as its meta keeps it
disposition := mime.FormatMediaType("attachment", map[string]string{"filename": download.Meta["name"]})
w.Header().Set("Content-Disposition", disposition)
```

**Documents over HTTP.** A client that retries an upload sends
`If-None-Match: *`, and the retry adds nothing; an editor saving sends the
ETag it read, and a save over someone else's is refused.

```go
func (a *app) putDocument(w http.ResponseWriter, r *http.Request) {
	options := []blobs.WriteOption{blobs.ContentType(r.Header.Get("Content-Type")), blobs.Size(r.ContentLength)}
	switch {
	case r.Header.Get("If-None-Match") == "*":
		options = append(options, blobs.IfNoneMatch())
	case r.Header.Get("If-Match") != "":
		options = append(options, blobs.IfMatch(r.Header.Get("If-Match")))
	}
	doc, err := a.documents.Put(r.Context(), r.PathValue("path"), http.MaxBytesReader(w, r.Body, 1<<30), options...)
	switch {
	case errors.Is(err, tinystore.ErrConflict):
		w.WriteHeader(http.StatusPreconditionFailed)
	case errors.Is(err, tinystore.ErrLimit):
		w.WriteHeader(http.StatusRequestEntityTooLarge)
	case err != nil:
		http.Error(w, "not saved", http.StatusInternalServerError)
	default:
		w.Header().Set("ETag", doc.ETag)
		w.WriteHeader(http.StatusCreated)
	}
}
```

## Beside the other engines

[examples/notes](../examples/notes/main.go) with a note's attachments: the
engine opens against the store after the others, `Close` closes it first, and
`backup.Write` takes its files with the rest.

```go
type app struct {
	// … the handles notes keeps now
	objects     *blobs.Store
	attachments *blobs.Bucket
}

// in openEngines, after the engines it opens now
if a.objects, err = blobs.Open(ctx, a.store, blobs.Options{}); err != nil {
	return err
}
if a.attachments, err = blobs.OpenBucket(ctx, a.objects, "attachments"); err != nil {
	return err
}

// useAttachments keeps a note's files beside its row: the row names the key and
// the bucket holds the bytes; a stale edit is refused as a stale draft is
func (a *app) useAttachments(ctx context.Context, out io.Writer) error {
	files := a.attachments.Of("notes", 1)
	list, err := files.Put(ctx, "list.txt", strings.NewReader("milk, bread, eggs, tea"),
		blobs.ContentType("text/plain; charset=utf-8"))
	if err != nil {
		return err
	}
	_, err = files.Put(ctx, "list.txt", strings.NewReader("milk"), blobs.IfMatch(`"read-long-ago"`))
	if !errors.Is(err, tinystore.ErrConflict) {
		return errors.Join(err, errors.New("a stale edit replaced the list"))
	}

	file, found, err := files.Open(ctx, "list.txt")
	if err != nil || !found {
		return errors.Join(err, errors.New("the list is missing"))
	}
	defer file.Close()
	head := make([]byte, 4)
	if _, err = file.ReadAt(head, 0); err != nil {
		return err
	}
	usage, err := files.Usage(ctx)
	fmt.Fprintf(out, "attachment %s: %d bytes, starts %q; note 1 holds %d\n", list.Key, list.Size, head, usage.Bytes)
	return err
}

// deleting a note deletes its row first, then its files
_, err = a.db.Exec(ctx, `delete from notes where id = ?`, id)
err = a.attachments.Of("notes", id).Clear(ctx)
```

## Keys

- **A key is a path**: segments separated by `/`, each non-empty, not `.` or
  `..`, UTF-8 without control characters; at most 16 segments and 1 KiB in
  all, owners included. It names nothing on the disk, where files are named by
  numbers, so a key is its own bytes everywhere: `Photo.jpg` and `photo.jpg`
  are two keys on NTFS and APFS too, and no key is refused for being a name
  Windows reserves.
- **`Of` names a folder.** Its owners are segments, a string or an integer
  each, an integer by its decimal text as in kv: `Of("users", 42)` is the
  folder `users/42/`. A key given to it may hold `/` of its own, so
  `media.Of("users", 42).Put(ctx, "photos/1.jpg", r)` and
  `media.Put(ctx, "users/42/photos/1.jpg", r)` write one object. An owner
  holding `/` is `ErrInvalid`. A folder is named whole, its path ending in
  `/`, so `Of("users", 4)` never meets user 42, which the text prefix
  `users/4` would.
- **A folder exists while it holds objects**; nothing creates or removes one.
- **An object's `Key` is its path under the handle that returned it**, so
  `media.Of(42).Open(ctx, obj.Key)` opens what `media.Of(42).Scan` listed. An
  error names the whole path.
- **`Scan` walks every key under its folder**, sub-folders included, in the
  byte order of the path, `10.jpg` before `9.jpg`, and with `Query.Prefix` the
  keys that start with it. kv's `Scan` reads a branch's own keys; a folder's
  own objects and its sub-folders, as a file browser shows them, are
  [not in the first version](#not-in-the-first-version).

## Objects

```go
type Object struct {
	Key         string            // its path under the handle that returned it
	Size        int64
	ETag        string            // quoted, as an HTTP header carries it
	ContentType string
	Modified    time.Time         // when this version was committed, by the store's clock
	Expires     time.Time         // zero when it does not expire
	Meta        map[string]string
}
```

- **The ETag is the bytes'.** It is derived from the SHA-256 computed while
  the bytes arrive, so two objects with the same bytes have one ETag and a
  write of other bytes has another. It is quoted, so
  `w.Header().Set("ETag", obj.ETag)` is a valid header. The derivation is not
  promised: an ETag is compared with ETags and nothing else.
- **A type is kept as it was given**, up to 256 bytes; the engine judges
  nothing. A `text/html` a stranger uploaded runs as a page of the origin that
  serves it, and an object stored without a type is typed by
  `http.ServeContent` from its first bytes, which can make it HTML: an
  application serving uploads answers a type it does not trust with
  `Content-Disposition: attachment`, or serves uploads from an origin of their
  own.
- **Meta is a few strings for serving**: a file's original name, a width. A
  key is `[a-z0-9][a-z0-9_-]{0,63}`, and keys and values together are at most
  2 KiB. What a query searches is a column of the application's.
- **`Modified` is the store's clock at the commit**, to the millisecond; a
  `Copy` or a `Move` commits a new version.

## Writing

- **`Put` returns once the object is durable**: its bytes synced and its row
  committed. Whatever the key held before is replaced whole at that moment.
- **An object appears at its commit, whole, or not at all.** No reader sees a
  part of one, under its key or under any other.
- **Memory does not follow size.** An upload holds at most the inline size in
  memory, 16 KiB, while it does not know whether its object is larger; past
  it, bytes go to the upload's file as they arrive. A 64-byte icon and a 17 GB film pass through one call and one
  buffer.
- **`blobs.Size(n)` says the stream's length.** The engine places the object
  before its first byte, and a stream that ends short of `n` or runs past it is
  `ErrInvalid` and leaves nothing. A negative `n` says nothing, so
  `blobs.Size(r.ContentLength)` needs no condition around it; a
  `*bytes.Reader` or a `*strings.Reader` says its own length.
- **An unfinished upload is not an object.** Its bytes wait in `uploads/`,
  where no reader looks, until its commit names them.
- **An upload lives as long as the context given to `Create`.** A request
  that ends mid-upload leaves nothing, and the store's `Close` aborts every
  upload that has not committed; an upload used after that, after `Commit` or
  after `Abort` is `ErrClosed`. An `Upload` is one goroutine's.
- **`MaxSize(n)` bounds a bucket's objects.** The upload that passes it stops
  with `ErrLimit` and leaves nothing; there is no bound by default, since a
  film is 17 to 80 GB.
- **`Options.KeepFree` keeps the disk from filling.** An upload that would
  leave less free, checked at `Create` for a declared size and every 64 MiB it
  writes, stops with `ErrLimit` and leaves nothing, so that one upload cannot
  take the space `kv.db` and `jobs.db` need to commit. 1 GiB by default.

## Conditions

- **`IfNoneMatch()` creates only.** A live object under the key is
  `ErrConflict`, so an upload retried after a timeout adds nothing.
- **`IfMatch(etag)` replaces or deletes only the version with that ETag**; any
  other, and an absent key, is `ErrConflict`.
- **A condition is checked twice**: when an upload begins, so that a doomed
  upload of gigabytes stops before its first byte, and in its commit, which
  decides.
- **An ETag is the bytes, not a version.** A key rewritten with the very bytes
  a writer read still matches its ETag, and the write it lets through replaces
  the bytes that writer saw, which is what the condition is for.

## Reading

- **`Open` returns the object as it is at that moment**, as a `*blobs.Reader`:
  `Read`, `ReadAt`, `Seek` and `Close`, and the `Object`'s fields. A reader of
  a file holds the file open and nothing of the engine's, so it goes on
  reading after its key is deleted, replaced or expired and after the store
  closes, until its own `Close`. `ReadAt` may be called from several
  goroutines; `Read` and `Seek` from one.
- **A range reads its own bytes.** `Seek` and `Read`, `ReadAt` or
  `io.NewSectionReader` over the reader take the bytes of the range from the
  file and nothing before them; an inline object's range is a slice of bytes
  already in memory.
- **`http.ServeContent` serves ranges and conditions** from the reader, its
  ETag and `Modified`, as the recordings case shows: `Range`, multiple ranges,
  `If-Range`, `If-None-Match`, `If-Modified-Since` and `HEAD`.
- **A whole read is checked** and a range is not; see
  [Integrity](#integrity).

## Expiry

- **`TTL(d)` and `ExpireAt(t)` on a write, `DefaultTTL(d)` on a bucket.** The
  store's clock, read once a call. An expiry is a schedule and not an
  observation, so blobs has no `ErrTooOld` window; `Options.Clock` moves a test
  a month ahead without sleeping.
- **Every write gives its object the expiry a `Put` would**: the call's, else
  the bucket's `DefaultTTL` from now, else none. An export generated again
  lives its day again, and a file moved out of `pending/` into a bucket
  without a default expires no more. This is not kv's rule, where a `Set`
  keeps the expiry a live key has.
- **Expired is absent to every operation.** `Open`, `Stat` and `Scan` do not
  see it, `IfNoneMatch` claims its key, and `IfMatch` conflicts with it.
- **Maintenance removes expired objects every minute**, 10,000 a transaction,
  and their bytes when no other key names them. Until then an expired object
  still counts in `Usage`.

## Copy, Move, Delete and Clear

- **`Copy` shares the bytes.** The destination is a new version holding the
  source's bytes, type and meta unless the call gives others; no byte is read
  or written, whatever the size. A `Copy` onto its own key gives it another
  type, meta or expiry and keeps its bytes.
- **`Move` is one write**: the destination is written and the source removed
  in one commit, the bytes untouched.
- **A `Copy` or `Move` from an absent key is `ErrConflict`**, as jobs'
  `Update` of an absent key is. Both stay inside one bucket, and both take
  `IfNoneMatch` for the destination.
- **`Delete` removes a key**; an absent key is not an error, and `IfMatch`
  makes it conditional.
- **`Clear` removes a folder and every folder under it**, at once. Up to
  10,000 objects go in one transaction; a larger folder is marked cleared at
  the file's revision, which hides its objects from every call the moment the
  mark commits, and maintenance deletes them 10,000 a transaction. An object
  written after the `Clear` is a new object and stays, as in kv.
- **Bytes leave when no key names them**: a file right after the commit that
  dropped its last name, and at the next maintenance if removing it failed.

## Usage

- **`Usage(ctx)` is the objects and bytes under a folder**, the bucket's own
  included, counted from the objects' rows over the folder's range of keys: a
  millisecond over ten thousand objects, 8 over a hundred thousand and 89 over
  a million in [the round](reports/blobs-mechanics-2026-09-27.md). No write
  pays for it. Totals kept for every folder would cost up to half the writes
  of deep paths under load, for a call many applications never make; they come
  when a workload calls `Usage` often over folders that large.
- **An expired object is absent from it at once**, as from every call, and so
  are the objects a `Clear` hid.
- **It counts objects, not files.** A `Copy` counts its bytes under its
  destination although no byte was copied.
- **A check is not a limit.** Two uploads that each pass a quota check can
  pass the quota together; the check before an upload with its declared
  `Size` is the application's, and a limit enforced inside the commit is
  [not in the first version](#not-in-the-first-version).

## Two files, no transaction

An application's row and the object it names are two commits, and the order
between them decides what a crash leaves:

- **Bytes before the row that names them.** A crash between leaves an object
  no row names, never a row naming an object that is not there.
- **The row deleted before its bytes**, for the same reason.
- **What a crash left is found by walking `All`** against the rows, or never
  made permanent: an upload that waits for its row under a `TTL`, and moves
  out when the row exists, expires when the row never comes.

## Deleting and replacing under readers

A file is renamed once, into `objects/` before any row names it, and never
after; it is never written after its commit and never given a name it had
before, so a reader that opened one reads exactly the object it looked up. The open file is the reader's lease: removing its name leaves the
bytes to the handles that have them, and the disk takes them back when the
last handle closes. [The earlier direction](storage-runtime-direction.md)
asked for leases that collection respects; a name that is never used twice
turns the race between a lookup and its open into a second lookup rather than
a wrong byte:

- **A file gone between the lookup and the open** was removed because its key
  changed: `Open` looks the key up again, three lookups in all, and then
  answers `ErrConflict`. A file gone while its key still names it is
  `ErrCorrupt` naming the key.

On Windows this holds only for files opened with `FILE_SHARE_DELETE`, which
`os.Open` does not ask for and `os.Root`'s opens do, as the Go 1.27.1 source
shows (`syscall.Open` against `internal/syscall/windows.Openat`). The probe on
27 September, Go 1.27.1, a 1 MiB file read halfway before the change and to
the end after it:

| A reader opened by | then | Windows 11, NTFS | Linux, the container |
|---|---|---|---|
| `os.Open` | `os.Remove` or `Root.Remove` | refused: the file is in use | removed; the reader reads every byte |
| `Root.Open` | `os.Remove` or `Root.Remove` | removed; the reader reads every byte; the name is free at once | the same |
| `Root.Open` | `Root.Rename` over it | replaced; the reader reads the old bytes | the same |
| `Root.Open` | `os.Rename` over it | refused: access is denied | replaced |
| `Root.Open` | `Root.Link`, then removing the first name | the link reads every byte | the same |

`os.Link` from outside the root, as a snapshot links, linked a file held
through either open on Windows.

- **The engine opens every file through an `os.Root` over `blobs/`**, which
  also keeps a path from leaving the directory. The gate on reading through a
  delete fails on Windows the day Go changes it, and a `syscall.CreateFile`
  asking for `FILE_SHARE_DELETE`, as the directory lock opens `LOCK`, then
  replaces it.
- **A file another program holds is removed later.** An antivirus scanner or
  a backup tool may open a file without sharing its deletion; the removal
  fails, the bytes stay listed for removal, and maintenance tries again and
  logs the failure once a quiet period.
- **A file system without POSIX deletion**, FAT or exFAT on Windows, keeps a
  removed name until its last handle closes; no name is used twice, so
  nothing waits for it but the space.

## Crash consistency

No transaction spans the file system and SQLite, so the order of a file's
writes is the whole protocol:

```text
step                                                    an exit after it leaves            the next Open
1  take the content's id from the block held in memory  nothing                            nothing
2  create uploads/1a3f07, write it, hash it             a partial file in uploads/         empties uploads/
3  sync the file                                        a whole file in uploads/           the same
4  rename it to objects/1a3/1a3f07, sync that directory a file in objects/ no row names    removes it: its id is past the settled mark
5  commit the content, the object, the replaced one     the object, and bytes no key       nothing: maintenance
                                                        names any more                     removes those bytes
6  remove the file of a content no key names            nothing                            nothing
```

- **A committed row never names bytes that are not durable**: the file and its
  name in `objects/` are synced before the commit that names them.
- **No write happens before the first byte.** A content's id comes from a
  block of a thousand reserved in `meta` by a transaction of its own, as jobs
  reserves its ids, so an upload names its file without a commit, and the
  round's cost of a row before the first byte, 3.8 ms alone on Linux, is gone.
- **An inline object is one commit**: its bytes, its row and the replaced
  content, together. So is a `Delete`, a `Copy` and a `Move`.
- **`Open` walks what a crash can have left, and nothing else**: `uploads/`,
  which holds the uploads in flight when the process died, 1024 at most; and
  the directories of `objects/` that hold the ids past the settled mark, the
  ids reserved since maintenance last moved it, where a file no content names
  is removed. Maintenance moves the mark up to the first id an upload still
  holds, so a crash leaves it at most a minute and the longest upload behind.
- **A commit whose outcome is unknown decides nothing wrongly**: the upload
  keeps its id unsettled, asks once the file answers whether a content names
  it, and removes its file when none does; a process that dies first leaves
  it to the next `Open`.
- **A rename can meet a scanner on Windows.** One that opened the new file
  without sharing its deletion makes the rename fail; the upload tries again a
  few times before it fails, and the gates show how often it must.
- **A process that dies and a disk that loses power are different tests.** The
  gates kill the process at every step; a power cut is argued from the order
  above and not tested, since no gate can cut the power.
- **Windows syncs a directory only when asked the right way.** Go's `Sync` of
  a directory is refused there (access is denied, in the probe); a directory
  opened through `syscall.CreateFile` with `GENERIC_WRITE` and
  `FILE_FLAG_BACKUP_SEMANTICS` flushes, in about a millisecond, so the engine
  opens it that way. SQLite's Windows VFS, as `modernc.org/sqlite` compiles
  it, flushes a file and never its directory.

## Integrity

- **Every content keeps its SHA-256**, computed as its bytes arrive: 2.4 GB/s
  of one core in the probe, on a Ryzen 7 7700, which has SHA instructions, so
  a 17 GB upload spends about seven seconds of a core on it. A processor
  without them is slower by a factor the round measures.
- **A whole read is checked.** A reader that reads from the first byte to the
  last in order compares the hash before it hands over its last bytes, so a
  whole read of a changed object fails before its end: `io.Copy` returns
  `ErrCorrupt`, and an HTTP client receives fewer bytes than `Content-Length`
  promised, which it treats as a failed download. `http.ServeContent` reads a
  whole object in order.
- **An inline object is checked before its first byte**: it is in memory whole,
  and hashing 64 KiB takes about 27 µs at the probe's rate.
- **A range is not checked.** Its bytes come from the file as they are; the
  scrub finds a change among them within its pass.
- **The scrub reads every content once a pass**, 30 days as its target
  until its pace is measured beside readers, a slice each minute, and keeps its place and its hash state in `blobs.db`, so a
  restart does not start it again. A changed or missing content is marked,
  logged once at Error with the keys that name it, and its next `Open` is
  `ErrCorrupt` naming the key; `Put`, `Delete` and `Clear` work over it as over
  any other. A backup is what repairs it.
- **A file is the object's bytes and nothing else**, so `sha256sum`, `ffprobe`
  and a hard link read it as it is.

## Storage

Settled by [the round](reports/blobs-mechanics-2026-09-27.md) and the user.
The engine claims `blobs/`, a directory, where every other engine claims a
file.

```text
data/blobs/
├── blobs.db          buckets, objects, contents, the bytes kept inline
├── uploads/
│   └── 1a3f08        an upload's bytes until its commit; emptied at open
└── objects/
    ├── 1a3/
    │   └── 1a3f07    content 0x1a3f07: the object's bytes, renamed here once, then never changed
    └── 1a4/          a directory for each 4,096 ids, so none holds more names than that
```

**Two places, not three.** A content up to the inline size is a row of
`bodies`, written in the transaction that names it; a larger one is a file of
its own. Packs, append-only files that many objects share, are what Haystack,
SeaweedFS and git keep small objects in, and they cost compaction: copying the
live objects out of a pack full of deleted ones, beside writers and readers.
The probe did not find them worth it here. Random bytes, one writer first,
microseconds an object at the median:

| Size | Windows: a file, synced | a pack, synced each | a pack, synced per 32 | container: a file and its directory, synced | a pack, synced each | per 32 |
|---:|---:|---:|---:|---:|---:|---:|
| 4 KiB | 1,547 | 1,505 | 47 | 3,124 | 3,067 | 96 |
| 64 KiB | 1,535 | 1,131 | 66 | 3,060 | 3,078 | 128 |
| 256 KiB | 1,547 | 1,444 | 145 | 3,412 | 3,169 | 197 |
| 1 MiB | 1,740 | 1,551 | 447 | 4,094 | 3,474 | 439 |

- **A file costs its sync, and so does an append to a pack.** What a pack
  gains is a sync shared by the objects before it, which needs objects
  arriving together.
- **Files arriving together share the disk's work too.** With 128 writers in
  the container, a file each reached 3,600 to 7,000 objects a second at 64
  KiB against 5,600 to 12,500 for one pack all writers appended to and one
  sync covered (two runs); at 256 KiB 1,800 against 3,400, at 1 MiB 1,300
  against 1,500 (one run). ext4 committing its journal once for concurrent
  syncs would explain it; that is a hypothesis.
- **On Windows the shared pack lost**: files held about 1,000 a second from 8
  writers up, at 64 KiB to 1 MiB, and the pack 750 at 64 and 256 KiB whatever
  the number of writers, as if every append waited for the flush before it.
  Why is a hypothesis: NTFS may hold a file's writes while it flushes it, and
  Go lets one handle write, or read, one call at a time there, since its
  Windows `Pread` and `Pwrite` take the file's read-write lock
  (`internal/poll`). A pack that batches its appends before it flushes, as the
  one-writer rows above did, would do better; the round measures one.
- **So the first version keeps files**, and inline covers the sizes where a
  sync an object hurts most, since an inline write shares its commit with the
  writes beside it, as kv's do: 53,000 durable `Set`s a second in the
  container at 512 callers in [the kv round](reports/kv-mechanics-2026-09-26.md).
  A content's place is a column, so packs can take a range of sizes later, a
  group's leader syncing the pack before its commit, without a call changing.

**Chunk rows lose.** Fixed chunks in SQLite would let a range read only its
chunks, which a file does already; every byte would go through the WAL twice,
every snapshot would copy all of them with `blobs.db`, and deleting them would
leave the file its size.

**The inline size is 16 KiB.** Up to it inline wrote and read faster than a
file on Linux and on Windows at every concurrency the round tried; at 64 KiB a
file read faster on both. Inline costs every byte written twice, to the WAL and
then to the file, a copy in every snapshot of `blobs.db`, and a body read
whole: 64 KiB is a chain of sixteen overflow pages, and the driver cannot read
part of one, since `modernc.org/sqlite` v1.59.0 hands out no incremental BLOB
I/O (`sqlite3_blob_open` is in its `lib` and nowhere in the driver, as
`bench/blob` found). A file costs a sync of its own and its directory's, and
an open a read. Nothing else the driver ships changes this:
`vfs`, `pcache`, `vtab` and `vec` answer other questions, and each would be an
import the size probe weighs.

```text
buckets    id | name
objects    bucket | path | revision | content | size | etag | type | modified | expires | meta
           without rowid, key (bucket, path); index (expires, bucket, path) where expires is not null
contents   id | names | size | sha256 | inline | damaged      names 0: bytes to remove
           index (id) where names = 0
bodies     id | bytes                                          an inline content's bytes
cleared    bucket | prefix | revision                          a Clear past 10,000 objects, as kv marks a branch
meta       name | value                                        the revision's high-water mark, the ids reserved
                                                               and settled, the scrub's place
```

- **4 KiB pages**, as kv and jobs have: an object's row holds its path, a few
  numbers, its type and its meta, and a `Scan` or a `Stat` never reads an
  inline body, which is a row of `bodies`; the round's `dbstat` says what each
  table costs.
- **A content's id names its file and is never given twice**, not after a
  delete or a crash: ids come from blocks reserved in `meta`, and every id at
  or below the settled mark is committed or has no file. A write's revision is
  apart from it, the file's high-water mark in the write's own transaction as
  kv's is.
- **A content counts its names**: `Copy` adds one, a `Delete`, a replacement,
  an expiry and a `Clear` take one away, and a content without names is
  removed, an inline one inside that transaction and a file after it.
- **A write's statements run in a savepoint of a grouped transaction**,
  through `internal/sqlite.File.UpdateGrouped` as kv's do: 1024 writes and 8
  MiB a group, so 128 inline objects of 64 KiB share one commit. A file's
  commit joins a group; its sync cannot, and its directory's is shared by the
  uploads that finish together in one directory.
- **A lookup is one statement** through `File.Lookup`, its own snapshot:
  `Open` and `Stat` read the object, its content and, when inline, its bytes
  together. A `Scan` page is one snapshot of at most five seconds.
- **Maintenance runs through `Store.Every`**: expired objects, objects a mark
  hid, files no key names, uploads whose context ended, the settled mark, and
  a slice of the scrub.

## Snapshot and backup

- **A snapshot links files and copies `blobs.db`.** It holds collection, copies
  `blobs.db` with `VACUUM INTO` as every engine copies its file, hard-links
  every file the copy names into the snapshot, and lets collection go. A link
  copies no byte, and its bytes stay after the live name is removed; a 17 GB
  film costs the snapshot a directory entry. Where a link is refused, on FAT or
  exFAT, the file is copied, and the snapshot needs its space.
- **A missing file is left out and logged**, and the restored store reports
  it as the original did: `ErrCorrupt` naming its key.
- **A backup is one zip, as now.** Each linked file is an entry, and
  `archive/zip` writes and reads by itself the zip64 records that an entry
  past 4 GiB, or an archive of a hundred thousand entries, needs. The manifest
  carries each file's size and SHA-256; `Restore` checks each and writes it
  beside its name before renaming it, as it does every file now.
  `zip.NewReader` holds every entry's header in memory, so a restore of a
  million files holds a million headers; the round measures what they cost.
- **Blob entries are stored, not deflated.** Media does not compress. Go 1.27
  deflated random bytes at 7.0 to 7.4 GB/s in the probe, so storing saves CPU
  rather than making anything possible.
- **Files are named once and never change**, so a tool that copies the
  snapshot, rsync or restic, copies each file once over many backups; the zip
  is for moving a whole store.

## Memory and admission

| Work | Holds | Reserved from the store's memory |
|---|---|---|
| an upload | the inline size, until it ends | at `Create` or `Put` |
| a reader of a file | its handle | nothing |
| a reader of an inline object | its bytes | at `Open`, until `Close` |
| a `Scan` page | 1000 objects, 4 MiB of their fields | before its read |
| the scrub | a buffer of 1 MiB | for its slice |

- **Uploads at once are bounded**: 1024 slots, then an upload waits for a slot
  or for memory, and leaves when its context ends. Grouped writes wait in 2048
  slots, as kv's do. Readers hold handles and no memory; the operating system
  bounds the handles.
- **Nothing the engine starts outlives a call.** An upload runs in its
  caller's `Write` and `Commit` calls, and maintenance through `Store.Every`.

## Bounds

| Object | Limit |
|---|---:|
| A path, owners and key | 1 KiB, 16 segments |
| An object | the disk's; `MaxSize(n)` a bucket's |
| Objects a bucket holds | the disk's |
| A content type | 256 bytes |
| Meta | 2 KiB |
| Kept inline | 16 KiB |
| Memory an upload holds | the inline size, 16 KiB |
| Uploads at once | 1024 |
| Free space kept | 1 GiB by default, `Options.KeepFree` |
| A `Scan` page | 1000 objects, 4 MiB |
| A `Clear` in its own transaction | 10,000 objects; a larger one is marked |
| Expired objects a maintenance transaction removes | 10,000 |
| Lookups an `Open` makes while its key changes | 3 |
| A scrub pass | 30 days as its target, measured before it is promised |

## Errors

| Sentinel | When |
|---|---|
| `ErrInvalid` | a path that is not one: empty, over 1 KiB or 16 segments, a segment empty, `.`, `..`, not UTF-8 or holding a control character; an owner holding `/`; a type over 256 bytes or meta over 2 KiB or a meta key outside its alphabet; a stream shorter or longer than its `Size`; both `TTL` and `ExpireAt`; a bucket name that is not `[a-z0-9][a-z0-9_-]{0,63}` |
| `ErrLimit` | an object past `MaxSize`, an upload that would leave less than `KeepFree` free, a page past its bound |
| `ErrConflict` | `IfMatch` against another ETag or an absent key, `IfNoneMatch` against a live object, a `Copy` or `Move` from an absent key, a key replaced three times while `Open` looked for its file |
| `ErrClosed` | the store closed, an upload `Close` aborted, an upload used after `Commit` or `Abort` |
| `ErrCorrupt` | bytes that do not hash to their content's SHA-256, a file missing while its key names it, a row that no longer decodes |

An error about one object is a `*blobs.KeyError` naming its bucket and whole
path; `errors.Is` finds the store's sentinel in it. A commit whose outcome is
unknown is `blobs.ErrOutcomeUnknown`, as kv's and jobs' are: the object may or
may not be there, and its caller `Stat`s it before writing again.

## Gates

The five cases are the gates' workloads.

| Promise | What enforces it |
|---|---|
| a `Put` that returned survives an abrupt exit, inline and in a file | `TestAPutThatReturnedSurvivesAnAbruptExit` |
| an object appears whole at its commit or not at all | `TestAnObjectAppearsWholeAtItsCommitOrNotAtAll` |
| an exit at every step of an upload leaves no row naming missing bytes, and no file the next open keeps | `TestAnExitAtEveryStepOfAnUploadLeavesNothingBehind` |
| an upload aborted, abandoned or ended by its context leaves nothing | `TestAnAbortedOrAbandonedUploadLeavesNothing` |
| the next open empties `uploads/` and walks only the unsettled ids of `objects/` | `TestOpenRemovesWhatAbandonedUploadsLeft` |
| a commit whose outcome is unknown leaves no file no row names | `TestAnUnknownCommitLeavesNoFileBehind` |
| a reader keeps what it opened through a delete, a replace, an expiry and a `Clear`, on Windows too | `TestAReaderKeepsWhatItOpened` |
| an `Open` racing a replace opens the new object | `TestAnOpenRacingAReplaceOpensTheNewObject` |
| a whole read of a changed byte fails before its end | `TestAWholeReadOfAChangedByteFailsBeforeItsEnd` |
| the scrub finds a changed or missing file and names its keys | `TestTheScrubNamesTheKeysOfWhatChanged` |
| a range reads its bytes and no others | `TestARangeReadsItsBytesAndNoOthers` |
| `http.ServeContent` answers ranges and conditions from a reader | `TestServeContentAnswersRangesAndConditions` |
| memory does not grow with an object's size | `TestMemoryDoesNotGrowWithAnObjectsSize` |
| uploads wait for their slots and memory, and leave when their context ends | `TestUploadsWaitForTheirSlotsAndMemory` |
| `IfNoneMatch` creates once | `TestIfNoneMatchCreatesOnce` |
| one of two conditional replaces conflicts | `TestOneOfTwoConditionalReplacesConflicts` |
| an expired object is absent to every operation | `TestAnExpiredObjectIsAbsentToEveryOperation` |
| a copy or a move takes the expiry a `Put` would give it | `TestACopyOrMoveTakesTheExpiryOfAWrite` |
| a copy shares the bytes and outlives its source | `TestACopySharesTheBytesAndOutlivesItsSource` |
| `Usage` counts what every write, move, delete, expiry and `Clear` left | `TestUsageCountsWhatIsThere` |
| `Clear` empties a folder and those under it at once, over the bound and under it | `TestClearEmptiesAFolderAndThoseUnderIt` |
| a key is its own bytes on every file system | `TestAKeyIsItsOwnBytesOnEveryFileSystem` |
| a path that is not one is refused | `TestAPathThatIsNotOneIsRefused` |
| a stream that disagrees with its `Size` is refused | `TestAStreamThatDisagreesWithItsSizeIsRefused` |
| an upload past `MaxSize` or `KeepFree` stops and leaves nothing | `TestAnUploadPastItsBoundsStopsAndLeavesNothing` |
| a file another program holds is removed later | `TestAFileHeldElsewhereIsRemovedLater` |
| a snapshot links the files and copies `blobs.db` | `TestASnapshotLinksFilesAndCopiesTheDatabase` |
| collection waits for a snapshot | `TestCollectionWaitsForASnapshot` |
| a backup restores every object bit for bit | `TestABackupRestoresEveryObject`, one past 4 GiB opt-in |
| the engine links no `net/http` | `TestBlobsImportsNoHTTP` |
| engines never import each other | `TestEnginesDoNotImportEachOther`, with blobs in its list |

## What the runtime gains

- **An engine snapshots several files.** `Snapshotter.Snapshot` returns one
  `SnapshotFile`; blobs returns `blobs.db` and every file it linked, and the
  manifest lists each.
- **The backup stores what does not compress**, and opens files through an
  `os.Root`: a file the backup holds through `os.Open` cannot be removed on
  Windows until the backup moves past it.
- **The architecture's blobs paragraph changes**: a reader's lease is its open
  file, and the snapshot is what collection waits for.
- **`AGENTS.md` changes with the code**: blobs in the status, the shape and
  the gates, and in `TestEnginesDoNotImportEachOther`.

## Not in the first version

Each waits for a workload that needs it and a measurement that pays for it.

- Resuming an upload after a lost connection or a restart: a row of its own
  would keep its offset and its SHA-256 state, which `crypto/sha256` marshals,
  and the protocol's `write` would carry the offset, as tus does.
- Packs for the sizes above inline, with their compaction; a snapshot would
  link a pack as it links a file and read it to the length its copy of
  `blobs.db` names, since a pack only grows. What would bring them is a store
  of a million small files, whose zip backup took 183 s and whose restore held
  198 MiB of headers in the round.
- Totals kept for every folder, for a workload that calls `Usage` often over
  folders of a million objects.
- Ranges that are checked: a CRC-32C a 64 KiB block, which would cost each
  reader a 64 KiB buffer and each file a table of its own.
- Sharing the bytes of equal uploads: contents found by their SHA-256 and
  counted by their names, as `Copy` counts them.
- Compression, which media does not repay.
- A folder's own objects and its sub-folders, as S3's delimiter lists them.
- Versions kept after a replace.
- A quota enforced inside the commit.
- Transactions over several blob calls, and `Copy` or `Move` between buckets.
- Appending to an object, and reading one while it is written.
- A walk of every directory for files no row names: only something outside
  the engine makes one, since the next `Open` walks what a crash can leave.
- Signed URLs, resizing, HTTP handlers: the application's.

## Decided

With the user on 27 September, after [the round](reports/blobs-mechanics-2026-09-27.md):

| Question | Decided | Why |
|---|---|---|
| the inline size | 16 KiB | inline won both ways up to it on both systems; at 64 KiB a file read faster; 32 KiB is not worth a round |
| `Usage` | a count over the folder's rows, no totals kept | 10⁵ objects in 8 ms, against up to 47 % of the writes of deep paths for totals |
| an upload's first row | none: ids reserved in blocks | a commit before the first byte cost 3.8 ms alone on Linux; an unfinished upload is not an object |
| packs | none in the first version | 1.3 to 1.9 times on Linux, a loss on Windows, and compaction's whole class of trouble; a million small files is the trigger for later |
| places | two: inline and a file | the round |
| keys | paths, `Of` naming a folder as kv names a branch | a path is what URLs, S3 and file systems speak, and `Of` makes `users/4` unable to meet `users/42` |
| what `Scan` walks | every key under the folder | reconciling with the application's rows and deleting an account need every key; kv's `Scan`, own keys only, differs here |
| a replacement's expiry | every write gets its own term | an export generated again lives its day again |
| `KeepFree` | 1 GiB by default | one upload must not take the space every engine's commits need |
| the scrub | on, 30 days as its target | its pace is measured beside readers before the default is promised |
| checked ranges | not in the first version | a file stays the object's bytes; a whole read is checked at 2 GB/s |
| the ETag | the bytes' hash | HTTP and S3 speak it |
| `MaxSize` | no bound by default | a film is 17 to 80 GB |
| resuming uploads, what `Put` returns, a download's name, absence, an upload's life, `Copy` and `Move` | as the draft proposed | starting again, the `Object`, a string of `Meta`, a found flag, the context given to `Create`, inside one bucket |

## Open

| Question | What decides it |
|---|---|
| the scrub's pace | `Open`'s p99 while the scrub reads at 30 days a pass, on both systems |
| how often a rename on Windows meets a scanner | the gates' count of retried renames with Defender's real-time protection on |
