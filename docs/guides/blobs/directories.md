# Directories

A directory is a folder on the store's machine, such as a film library, a
photo archive or a dataset, that the Blobs engine lists and reads but never
writes. A scan writes down which files appeared, changed or went since the
last scan; your program follows those changes from a cursor and serves the
files from the disk the way it serves stored files. Use one when the files
are too large to copy into `blobs/` and something else owns them.

```ts
const movies = store.directory('movies', { path: 'D:/Movies' })

const scan = await movies.scan() // { added: 3, changed: 1, removed: 0, skipped: 0, count: 1842, size: 2104332118016 }
const page = await movies.changes({ after: cursor }) // { items, next }
const file = await movies.get('Dune (2021)/Dune.mkv') // StoredFile | undefined
```

```python
movies = store.directory("movies", path="D:/Movies")

scan = await movies.scan()  # Scan(added=3, changed=1, removed=0, skipped=0, count=1842, size=2104332118016)
page = await movies.changes(after=cursor)  # Changes(items, next)
file = await movies.get("Dune (2021)/Dune.mkv")  # StoredFile | None
```

```go
movies, err := blobs.OpenDirectory(store, "movies", "D:/Movies")

scan, err := movies.Scan(ctx)                           // blobs.Scan{Added, Changed, Removed, Skipped, Count, Size}
page, err := movies.Changes(ctx, blobs.After(cursor))   // blobs.Changes{Items, Next}
file, found, err := movies.Get(ctx, "Dune (2021)/Dune.mkv") // *blobs.StoredFile
```

```rust
let movies = store.directory("movies").path("D:/Movies").open()?;

let scan = movies.scan()?; // Scan { added, changed, removed, skipped, count, size }
let page = movies.changes().after(&cursor).page()?; // Changes { items, next }
let file = movies.get("Dune (2021)/Dune.mkv")?; // Option<StoredFile>
```

A directory's name is a set of files' name, `[a-z0-9][a-z0-9_-]{0,63}`, and
the two never share one. The path is not kept in the store: a folder that
moved to another disk is opened with its new path, and its files are the
files it listed before. Opening walks nothing, so a new directory lists no
file until its first scan.

## Scan on a period

```ts
const photos = store.directory('photos', { path: '/mnt/photos', every: '15m', ignore: ['*.part', '.*'] })
```

```python
photos = store.directory("photos", path="/mnt/photos", every="15m", ignore=["*.part", ".*"])
```

```go
photos, err := blobs.OpenDirectory(store, "photos", "/mnt/photos", blobs.Every(15*time.Minute), blobs.Ignore("*.part", ".*"))
```

```rust
let photos = store.directory("photos").path("/mnt/photos").every(Duration::from_secs(15 * 60)).ignore(["*.part", ".*"]).open()?;
```

`every` runs a scan on the store's background work, the first a period after
the open; without it your program scans when it chooses, from a job's cron
or at its start. A period is minutes: a scan that finds nothing over a
million files costs about half a percent of a core every 15 minutes.
`ignore` names files and folders a scan passes over, `*` for any run of
characters within one name; a folder passed over is not entered.

A scan compares each file's size and modification time with what the last
scan wrote down, as rsync's quick check does. A file changed in place with
its size and time both kept is not seen.

## Follow the changes

```ts
let cursor = await state.get('movies.cursor') // yours, in kv or your database
for (;;) {
	const page = await movies.changes({ after: cursor })
	for (const { path, change, info } of page.items) await catalog.apply(path, change, info)
	if (page.items.length === 0) break
	cursor = page.next
	await state.set('movies.cursor', cursor)
}
```

```python
cursor = await state.get("movies.cursor")  # yours, in kv or your database
while True:
    page = await movies.changes(after=cursor)
    for item in page.items:
        await catalog.apply(item.path, item.change, item.info)
    if not page.items:
        break
    cursor = page.next
    await state.set("movies.cursor", cursor)
```

```go
for {
	page, err := movies.Changes(ctx, blobs.After(cursor))
	if err != nil {
		return err
	}
	for _, item := range page.Items {
		catalog.Apply(ctx, item.Path, item.Change, item.Info) // blobs.Added, blobs.Changed, blobs.Removed
	}
	if len(page.Items) == 0 {
		break
	}
	cursor = page.Next
}
```

```rust
loop {
    let page = movies.changes().after(&cursor).page()?;
    for item in &page.items {
        catalog.apply(&item.path, item.change, item.info.as_ref())?; // Change::Added, Changed, Removed
    }
    if page.items.is_empty() {
        break;
    }
    cursor = page.next;
}
```

A page holds up to 1000 changes in the order the scans wrote them, and
`next` is always there. Without `after`, the changes start from the first
file the directory ever listed, each `added`. A path is told once for what
it is now: added and then changed is one `added`, and added and removed
before you read is one `removed`, which a program that never saw it ignores.
There is no `moved`: a renamed file is `removed` at one path and `added` at
another.

A removed file's line is kept 30 days. A cursor older than that fails
`too old`: read the changes again without `after`, which tells every file
there now as `added`, and drop what you kept that was not told.

## Read a file

```ts
const file = await movies.get('Dune (2021)/Dune.mkv')
return new Response(file.slice(start, end).stream(), { status: 206, headers: { etag: file.etag } })

const info = await movies.head('Dune (2021)/Dune.mkv') // FileInfo | undefined
const films = await movies.folder('Dune (2021)').list() // { files, next }
const { count, size } = await movies.usage()
```

```python
file = await movies.get("Dune (2021)/Dune.mkv")
info = await movies.head("Dune (2021)/Dune.mkv")  # FileInfo | None
films = await movies.folder("Dune (2021)").list()  # FilePage(files, next)
usage = await movies.usage()  # Usage(count, size)
```

```go
file, found, err := movies.Get(ctx, "Dune (2021)/Dune.mkv")
http.ServeContent(w, r, "", file.LastModified, file)
info, found, err := movies.Head(ctx, "Dune (2021)/Dune.mkv")
films, err := movies.Folder("Dune (2021)").List(ctx)
```

```rust
let file = movies.get("Dune (2021)/Dune.mkv")?; // Option<StoredFile>, Read + Seek
let info = movies.head("Dune (2021)/Dune.mkv")?; // Option<FileInfo>
let films = movies.folder("Dune (2021)").list().page()?;
let usage = movies.usage()?;
```

These are a set of files' calls with a set of files' answers, so whatever
serves a stored file serves a film: ranges, `If-None-Match`, Go's
`ServeContent`. `list`, `head` and `usage` answer from the list the last scan
wrote and touch no disk. `get` reads the disk, and only a path the list
holds: a file gone since the scan is not found, and paths compare byte for
byte, so on a disk that folds case `dune.mkv` does not find `Dune.mkv`.

The ETag names a version, not the bytes: `"d-<size>-<time>"`, both in hex.
The content type is `application/octet-stream` and `meta` is empty, since
nobody wrote the file through the store. A read holds to the version `get`
opened: each piece looks at the file's size and time, and once the file
moved the read fails `conflict`, so that a range never joins two versions of
one film. A file small enough to come whole with `get` is already in memory
and stays the version it was.

## Files still being written

```ts
const recordings = store.directory('recordings', { path: '/srv/camera', growing: true })
```

```python
recordings = store.directory("recordings", path="/srv/camera", growing=True)
```

```go
recordings, err := blobs.OpenDirectory(store, "recordings", "/srv/camera", blobs.Growing())
```

```rust
let recordings = store.directory("recordings").path("/srv/camera").growing(true).open()?;
```

In a `growing` directory a read holds to no version: it goes up to the size
the file has as it reads, past the size `get` gave, and each piece brings the
file's size and time, which the Rust and Go file's size and time follow. No
read can tell which bytes have arrived: a torrent's file is its whole length
from the start.

## Over a server

```sh
tinystore serve ./data --listen tls://0.0.0.0:7443 --tls-cert c.pem --tls-key k.pem --tokens tokens --directory movies=/mnt/media
```

A program on the machine, in its own process, a private child or a sidecar,
names a path with its own rights. A connection that came with a token names
none: it opens a directory by its name alone, which is the server's
`--directory` of that name, and a path or a name the server was not given is
`invalid`. A token that only reads lists, reads and follows changes, and
cannot scan.

```ts
const movies = store.directory('movies') // over a token: the server's own folder
```

```python
movies = store.directory("movies")  # over a token: the server's own folder
```

```go
movies, err := blobs.OpenDirectory(store, "movies", "") // over a token: the server's own folder
```

```rust
// Rust opens the store in its own process, which always names its path.
```

## What it promises

- The store never writes, renames or removes anything under the path
  (contract BLOB-10).
- A file that appeared, went, or whose size or time changed is told by the
  next scan of its folder, and a reader that follows its cursor, or walks
  from the start, ends with what the list holds (contract BLOB-11).
- What a scan could not read is not gone: a folder it cannot read keeps the
  files it listed, a folder on another file system is not entered, and a scan
  of a missing or unreadable folder, or of one found empty while its list
  holds files, fails `unavailable` and changes nothing (contract BLOB-12). A
  disk not mounted is an empty folder; `forget()` empties the list of a
  folder that was truly emptied.
- A read hands back only bytes of the version its `get` opened (contract
  BLOB-13).
- No read leaves the folder, through a link or a path swapped for a link
  between the scan and the read: a read opens its path a segment at a time,
  following no link (contract BLOB-14). A scan itself walks by paths, so a
  folder swapped for a link while a scan walks it can list names from
  outside the folder; reading them is refused all the same.
- A connection that came with a token names no path of the server's machine
  (contract BLOB-15).
- A scan's memory does not follow the number of files: a folder of a million
  files is sorted through a temporary table a few thousand names at a time
  (contract BLOB-16).

Links are not followed: a symbolic link or a junction is neither listed nor
entered, and counts in `skipped`, with a folder it cannot read, a folder on
another disk, a name that is not text and a path past the bounds. The store
logs the first of each kind once for each open directory.

## Errors

| Error         | When                                                                                 | What to do                         |
|---------------|--------------------------------------------------------------------------------------|------------------------------------|
| `invalid`     | a bad name, path or pattern; a name a set of files has; a path over a token          | fix the call                       |
| `unavailable` | a scan of a folder that is missing, unreadable, or empty while its list is not       | bring the disk back and scan again |
| `unavailable` | a directory opened without a path, or on a node's copy a file the list does not have | read it on the home                |
| `conflict`    | a read of a file that changed since its `get`, in a directory that is not growing    | `get` again                        |
| `too old`     | a cursor of changes older than 30 days                                               | read the changes without `after`   |
| `io`          | the disk failed under a read                                                         | read again                         |

## Limits and defaults

| What                  | Bound                                     |
|-----------------------|-------------------------------------------|
| a path                | 1 KiB and 16 segments, as a stored file's |
| a page of changes     | 1000 items                                |
| a removed file's line | 30 days                                   |
| `ignore`              | 64 patterns of 256 bytes                  |
| `every`               | none by default                           |
| files of a directory  | none                                      |

A backup holds a directory's list and none of its bytes. A restored store's
directory lists what it listed, and reads nothing until it is opened with a
path on that machine.

## See also

- [Serving files](serving.md): ETags, ranges and `ServeContent`, the same
  for a directory's files.
- The design, in research's
  [tinystore/design](https://github.com/tinyshed/research/tree/main/tinystore/design).
