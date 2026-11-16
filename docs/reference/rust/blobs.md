# Blobs API for Rust

Every public type, trait and function of the Blobs engine in `tinystore::blobs` and `tinystore::aio::blobs`, generated from its source. The [Blobs guide](../../guides/blobs/README.md) explains how to use them, and the [Bun and Node](../bun/blobs.md), [Python](../python/blobs.md) and [Go](../go/blobs.md) pages list the same API.

## Store

```rust
impl Store
```

### Store.directory

```rust
pub fn directory(&self, name: &str) -> DirectoryBuilder<'_>
```

The directory `name`: a folder of this machine, which the store lists
and reads and never writes. Its name is a set of files' too, and the
two never share one.

### Store.files

```rust
pub fn files(&self, name: &str) -> FilesBuilder<'_>
```

The set of files `name`, made the first time it opens: bytes by path,
written whole and replaced whole.

## aio::Store

```rust
impl aio::Store
```

### aio::Store.files

```rust
pub fn files(&self, name: &str) -> FilesBuilder<'_>
```

The set of files `name`, made the first time it opens, as
`crate::Store::files` opens it.

## SetMaxSize

```rust
pub struct SetMaxSize<'f> { /* private fields */ }
```

A change of a set's ceiling, its condition before `SetMaxSize::apply`.

### SetMaxSize.if_current

```rust
pub fn if_current(self, bytes: u64) -> Self
```

Changes the ceiling only while the set keeps `bytes`: another is a
conflict, so that two deploys never silently undo each other.

### SetMaxSize.when_full

```rust
pub fn when_full(self, when_full: WhenFull) -> Self
```

What a write past the new ceiling does; what the set does now when
not given.

### SetMaxSize.apply

```rust
pub fn apply(self) -> Result<()>
```

Writes the ceiling, nothing when the set keeps it already.

## Trim

```rust
pub struct Trim {
    pub evictions: u64,
    pub freed: u64,
}
```

What a trim removed: its files, and their sizes as `usage` counts them.

## WhenFull

```rust
pub enum WhenFull {
    Evict,
    Refuse,
}
```

What a write that would take a set past its ceiling does.

## Change

```rust
pub enum Change {
    Added,
    Changed,
    Removed,
}
```

What became of a file since a cursor.

## Changes

```rust
pub struct Changes {
    pub items: Vec<FileChange>,
    pub next: String,
}
```

A page of changes in the order the scans wrote them, and the cursor to ask
from next: with no items, the one to ask from after the next scan.

## ChangesCall

```rust
pub struct ChangesCall<'d> { /* private fields */ }
```

A page of a directory's changes, its cursor before `ChangesCall::page`.

### ChangesCall.after

```rust
pub fn after(self, cursor: impl Into<String>) -> Self
```

The changes past this cursor, a page's `next`.

### ChangesCall.page

```rust
pub fn page(self) -> Result<Changes>
```

## Directory

```rust
pub struct Directory { /* private fields */ }
```

A directory, or a folder of it: what its scans listed, and its files read
from the disk.

### Directory.changes

```rust
pub fn changes(&self) -> ChangesCall<'_>
```

The changes of every file since a cursor, or from the first file the
directory ever listed, each `added`.

### Directory.list

```rust
pub fn list(&self) -> DirectoryList<'_>
```

A page of this folder's files and of the folders in it, in the byte
order of their paths, as the last scan wrote them down.

### Directory.usage

```rust
pub fn usage(&self) -> Result<Usage>
```

The files of this folder and of the folders in it, and their bytes, as
the last scan wrote them down.

### Directory.folder

```rust
pub fn folder(&self, segment: impl Key) -> Directory
```

The folder in this one that `segment` names, whose paths are relative
to it.

### Directory.scan

```rust
pub fn scan(&self) -> Result<Scan>
```

Walks the folder and writes down what appeared, changed or went since
the last scan. One scan of a directory runs at a time: a second waits
for the first and answers as it does.

### Directory.get

```rust
pub fn get(&self, path: &str) -> Result<Option<StoredFile>>
```

The file at the path as a scan listed it and as the disk holds it now:
none when no scan listed it, or when it is gone since.

### Directory.head

```rust
pub fn head(&self, path: &str) -> Result<Option<FileInfo>>
```

What the last scan wrote down of the file at the path; it reads no disk.

### Directory.forget

```rust
pub fn forget(&self) -> Result<()>
```

Removes every file of the list, each told `removed`, and nothing on
the disk: for a folder that was truly emptied, which a scan refuses to
believe.

## DirectoryBuilder

```rust
pub struct DirectoryBuilder<'s> { /* private fields */ }
```

What opens a directory: its folder on this machine, how often the store
scans it, the names it passes over, and whether its files grow as they
are read.

### DirectoryBuilder.path

```rust
pub fn path(self, path: impl AsRef<Path>) -> Self
```

The folder on this machine. It is not kept in the store: a directory
opened without one lists what its scans wrote and reads nothing.

### DirectoryBuilder.every

```rust
pub fn every(self, period: Duration) -> Self
```

Scans the folder on the store's background work, the first a period
after the open; a period is minutes, not seconds.

### DirectoryBuilder.ignore

```rust
pub fn ignore<I, S>(self, patterns: I) -> Self
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
```

Names a scan passes over, a file's or a folder's, `*` for any run of
characters within one name: a folder passed over is not entered.

### DirectoryBuilder.growing

```rust
pub fn growing(self, growing: bool) -> Self
```

For a folder whose files are written while they are read: a read holds
to no version, and goes up to the size the file has as it reads.

### DirectoryBuilder.open

```rust
pub fn open(self) -> Result<Directory>
```

## DirectoryList

```rust
pub struct DirectoryList<'d> { /* private fields */ }
```

A page of a folder's files, its options before `DirectoryList::page`.

### DirectoryList.prefix

```rust
pub fn prefix(self, prefix: impl Into<String>) -> Self
```

Only the files whose path within the folder starts with this text.

### DirectoryList.after

```rust
pub fn after(self, path: impl Into<String>) -> Self
```

The page that starts past this path, as a page's `next` says it.

### DirectoryList.limit

```rust
pub fn limit(self, files: usize) -> Self
```

### DirectoryList.page

```rust
pub fn page(self) -> Result<Page>
```

## FileChange

```rust
pub struct FileChange {
    pub path: String,
    pub change: Change,
    pub info: Option<FileInfo>,
}
```

One file of a page of changes: its whole path in the directory, and what
it carries now, none once it is removed.

## Scan

```rust
pub struct Scan {
    pub added: u64,
    pub changed: u64,
    pub removed: u64,
    pub skipped: u64,
    pub count: u64,
    pub size: u64,
}
```

What a scan found: the files it told added, changed and removed, what it
met and could not list, and what the list holds once it ended.

## FileCall

```rust
pub struct FileCall<'f> { /* private fields */ }
```

One path and the options of a call on it, which ends in its verb.

### FileCall.content_type

```rust
pub fn content_type(self, content_type: impl Into<String>) -> Self
```

### FileCall.meta

```rust
pub fn meta(self, name: impl Into<String>, value: impl Into<String>) -> Self
```

A string of the program's the file carries, such as the name it was
uploaded as.

### FileCall.ttl

```rust
pub fn ttl(self, ttl: Duration) -> Self
```

The file's own term, from its write.

### FileCall.if_match

```rust
pub fn if_match(self, etag: impl Into<String>) -> Self
```

Writes only over the file with this ETag: one that changed, expired or
went since is a conflict, and nothing is written.

### FileCall.size

```rust
pub fn size(self, bytes: u64) -> Self
```

How long the body is; one longer or shorter is refused and leaves
nothing.

### FileCall.timeout

```rust
pub fn timeout(self, timeout: Duration) -> Self
```

How long a `get_or_create`'s claim of the path is renewed while its
function runs: 10 minutes unless said. A function still running then
loses the claim within 30 s, and another call may run its own.

### FileCall.get_or_create

```rust
pub fn get_or_create<T, E>(self, work: impl FnMut() -> Result<T, E>) -> Result<StoredFile, E>
where
    T: Into<Fill>,
    E: From<Error> + Display + 'static,
```

The file at the path, or, on a miss, the one `work` makes, written
with the call's type and meta unless what it gives says its own.

```rust
let pages = store.files("pages").open()?;
let page = pages.key(&format!("{hash}.pdf")).content_type("application/pdf").get_or_create(|| convert(doc))?;
assert_eq!(page.info().content_type, "application/pdf");
```

While one call runs `work`, every other `get_or_create` of the path,
in this process or through its server, waits for it and gets the file
it wrote (BLOB-21). The call claims the path for 30 s and the store
renews the claim while `work` runs, up to its [`timeout`](Self::timeout);
what `work` gives lands only while the claim still holds the path and
no file is there (BLOB-22). A put of the path meanwhile wins, and the
calls get the put's file; a delete ends the claim, what `work` gave is
dropped, and one of the calls runs it again, so `work` may run more
than once. A failure of `work` is the answer of every call that waited
for it, an error of the store's as it is and any other `unavailable`
with its text, and the next call runs it again.

Inside a database's transaction, or on a node's copy of blobs.db, it
is `Invalid`; so is a call of the path inside its own `work`.

### FileCall.put

```rust
pub fn put(self, bytes: impl AsRef<[u8]>) -> Result<FileInfo>
```

### FileCall.create

```rust
pub fn create(self, bytes: impl AsRef<[u8]>) -> Result<bool>
```

Writes only when the path holds no file; says whether it did.

### FileCall.upload

```rust
pub fn upload(self) -> Result<Upload>
```

A file written a piece at a time, which appears at its `commit`.

### FileCall.delete

```rust
pub fn delete(self) -> Result<()>
```

### FileCall.copy_from

```rust
pub fn copy_from(self, from: &str) -> Result<FileInfo>
```

Writes the file at `from` here too, sharing its bytes.

### FileCall.rename_from

```rust
pub fn rename_from(self, from: &str) -> Result<FileInfo>
```

Moves the file at `from` here in one write.

## Files

```rust
pub struct Files { /* private fields */ }
```

A set of files by path, or a folder of it: bytes written whole and
replaced whole, each with a type, an ETag and a few strings of the
program's.

### Files.set_max_size

```rust
pub fn set_max_size(&self, bytes: u64) -> SetMaxSize<'_>
```

Changes the set's ceiling for every handle, in this process and in
every other, from its commit on; a lower one evicts at once.

### Files.remove_max_size

```rust
pub fn remove_max_size(&self) -> SetMaxSize<'_>
```

Removes the set's ceiling: it grows as its writes take it.

### Files.trim

```rust
pub fn trim(&self, to: u64) -> Result<Trim>
```

Removes files in the eviction's order until the set holds at most `to`
bytes, and leaves its ceiling where it was: a batch of 10,000 files a
commit, so that no trim holds the writer long.

### Files.folder

```rust
pub fn folder(&self, segment: impl Key) -> Files
```

The folder in this one that `segment` names: `folder("users").folder(42)`
is `users/42`, and its files' paths are its segments and then theirs.

### Files.key

```rust
pub fn key(&self, path: &str) -> FileCall<'_>
```

A call on one path, its options before its verb:
`files.key("a.md").if_match(&etag).put(&text)`.

### Files.put

```rust
pub fn put(&self, path: &str, bytes: impl AsRef<[u8]>) -> Result<FileInfo>
```

Writes the file and replaces whatever the path held, at the moment it
returns.

### Files.create

```rust
pub fn create(&self, path: &str, bytes: impl AsRef<[u8]>) -> Result<bool>
```

Writes the file only when the path holds none; says whether it did.

### Files.upload

```rust
pub fn upload(&self, path: &str) -> Result<Upload>
```

A file written a piece at a time, which appears at its `commit`.

### Files.get

```rust
pub fn get(&self, path: &str) -> Result<Option<StoredFile>>
```

The file at the path and the bytes it reads, which stay what they were
now through a replace, a delete or an expiry; none when there is none.

On a set with `idle` it renews the file's term, and on a set with a
ceiling it moves the file up the eviction's order.

### Files.head

```rust
pub fn head(&self, path: &str) -> Result<Option<FileInfo>>
```

What the file at the path carries, none of its bytes. A use, as a
`get` is: a conditional request answered `304` used the file.

### Files.delete

```rust
pub fn delete(&self, path: &str) -> Result<()>
```

Removes the file at the path; nothing to remove is not an error.

### Files.copy

```rust
pub fn copy(&self, from: &str, to: &str) -> Result<FileInfo>
```

Writes the file at `from` at `to` too, sharing its bytes: no byte is
copied, and the two are two files from then on. A file at `to` is a
conflict: `key(to).if_match(etag).copy_from(from)` replaces the version
it names.

### Files.rename

```rust
pub fn rename(&self, from: &str, to: &str) -> Result<FileInfo>
```

Moves the file at `from` to `to` in one write, as `copy` writes it.

### Files.expire

```rust
pub fn expire(&self, path: &str, after: Duration) -> Result<bool>
```

Makes the file at the path expire `after` from now; says whether there
was one.

### Files.list

```rust
pub fn list(&self) -> List<'_>
```

A page of this folder's files and of the folders in it, in the byte
order of their paths: `10.jpg` before `9.jpg`.

### Files.all

```rust
pub fn all(&self) -> All
```

Every file of this folder and of the folders in it, a page at a time,
holding nothing between pages.

### Files.usage

```rust
pub fn usage(&self) -> Result<Usage>
```

The files of this folder and of the folders in it, and their bytes, as
they are now, with the set's ceiling and what it evicted since the
store opened. It renews nothing.

### Files.clear

```rust
pub fn clear(&self) -> Result<()>
```

Removes this folder and every folder in it, at once for every reader,
however many files it holds. On the files themselves it empties them.

## FilesBuilder

```rust
pub struct FilesBuilder<'s> { /* private fields */ }
```

What opens a set of files: its name, the term and the bound of the files
written in it, and the ceiling of them all.

### FilesBuilder.ttl

```rust
pub fn ttl(self, ttl: Duration) -> Self
```

The term of every file written without its own, counted from its
write; not a ceiling.

### FilesBuilder.idle

```rust
pub fn idle(self, idle: Duration) -> Self
```

The term of every file written without its own, counted from its last
`get` or `head`, as a bucket's `idle` counts a key's; `ttl` and `idle`
exclude each other.

### FilesBuilder.max_file_size

```rust
pub fn max_file_size(self, bytes: u64) -> Self
```

The largest file a write may leave: one past it is refused and leaves
nothing, and nothing stored is ever removed to make room.

### FilesBuilder.max_size

```rust
pub fn max_size(self, bytes: u64) -> Self
```

The most the set's files may hold together: a write past it removes
the files used least recently until the set fits. The set keeps it, so
that it holds for every handle; an open with another fails `conflict`,
and `Files::set_max_size` changes it.

### FilesBuilder.when_full

```rust
pub fn when_full(self, when_full: WhenFull) -> Self
```

What a write past `max_size` does: evicts, or with
`WhenFull::Refuse` fails `limit` and removes nothing. Kept with the
ceiling.

### FilesBuilder.open

```rust
pub fn open(self) -> Result<Files>
```

## Fill

```rust
pub struct Fill { /* private fields */ }
```

What a `get_or_create`'s function gives: the file's bytes, whole, as a
reader or in a file of this machine, and the type and the meta it is
written with when the call gave none.

```rust
use tinystore::blobs::Fill;

let pages = store.files("pages").open()?;
let page = pages.key("7f3a.pdf").get_or_create(|| {
    let pdf = std::fs::File::open("/tmp/7f3a.pdf").map_err(|error| tinystore::Error::io("the page", error))?;
    Ok::<_, tinystore::Error>(Fill::reader(pdf).content_type("application/pdf")) // streamed, never whole in memory
})?;
```

### Fill.bytes

```rust
pub fn bytes(bytes: impl Into<Vec<u8>>) -> Fill
```

### Fill.reader

```rust
pub fn reader(reader: impl Read + Send + 'static) -> Fill
```

Bytes read to their end as they are written, a piece at a time.

### Fill.file

```rust
pub fn file(path: impl Into<PathBuf>) -> Fill
```

The bytes of a file of this machine, read as they are written.

### Fill.content_type

```rust
pub fn content_type(self, content_type: impl Into<String>) -> Self
```

The file's type, in place of the call's.

### Fill.meta

```rust
pub fn meta(self, name: impl Into<String>, value: impl Into<String>) -> Self
```

A string of the program's the file carries; any given here take the
place of the call's.

## FileInfo

```rust
pub struct FileInfo {
    pub path: String,
    pub size: u64,
    pub etag: String,
    pub content_type: String,
    pub last_modified: SystemTime,
    pub expires: Option<SystemTime>,
    pub meta: BTreeMap<String, String>,
}
```

A file's fields: what serving it needs, and nothing a query would search.

## Page

```rust
pub struct Page {
    pub files: Vec<FileInfo>,
    pub next: Option<String>,
}
```

A page of a folder's files, in the byte order of their paths, and where
the next page starts when one follows.

## Usage

```rust
pub struct Usage {
    pub count: u64,
    pub size: u64,
    pub max_size: Option<u64>,
    pub evictions: u64,
}
```

What a folder holds now: its files and their bytes, and the set's
ceiling and what it evicted since the store opened.

## All

```rust
pub struct All { /* private fields */ }
```

Every file of a folder, a page at a time, as `Files::all` walks them.

## List

```rust
pub struct List<'f> { /* private fields */ }
```

A page of a folder's files, its options before `List::page`.

### List.prefix

```rust
pub fn prefix(self, prefix: impl Into<String>) -> Self
```

Only the files whose path within the folder starts with this text:
`"photos/1"` finds `photos/10.jpg` too.

### List.after

```rust
pub fn after(self, path: impl Into<String>) -> Self
```

The page that starts past this path, as a page's `next` says it.

### List.limit

```rust
pub fn limit(self, files: usize) -> Self
```

### List.page

```rust
pub fn page(self) -> Result<Page>
```

## PAGE_FILES

```rust
pub const PAGE_FILES: usize
```

Files a page holds at most, and when a list does not say.

## Maintenance

```rust
pub struct Maintenance {
    pub expired: usize,
    pub cleared: usize,
    pub evicted: usize,
    pub removed: usize,
    pub scrubbed: u64,
    pub damaged: usize,
}
```

What one maintenance did.

## maintain

```rust
pub fn maintain(store: &Store) -> Result<Maintenance>
```

Removes expired files, the files large clears hid, and bytes no file names
any more, and scrubs a slice of the stored bytes. The store runs it every
minute; a store opened without background work leaves it to the caller.

## StoredFile

```rust
pub struct StoredFile { /* private fields */ }
```

A file and its bytes as `Files::get` found it. A whole read, from its
first byte to its last in order, is checked against the SHA-256 taken when
it was written, and its last bytes are held back until it matches; a range
is not checked.

A directory's file, as [`Directory::get`](super::Directory::get) opened
it, has no hash: each piece read holds to the size and the time it was
opened with, and fails `conflict` once the file moved. In a growing
directory a read holds to no version, and its [`info`](Self::info) takes
the file's size and time from each piece read in order.

### StoredFile.info

```rust
pub fn info(&self) -> &FileInfo
```

### StoredFile.read_all

```rust
pub fn read_all(&mut self) -> Result<Vec<u8>>
```

The whole file from its first byte, checked; it holds every byte in
memory, so a large file is read as a stream.

### StoredFile.read_at

```rust
pub fn read_at(&self, bytes: &mut [u8], offset: u64) -> Result<usize>
```

Reads at `offset` without moving where the next read begins, from
several threads at once; like any range it is not checked.

## TxFiles

```rust
pub struct TxFiles<'a> { /* private fields */ }
```

A set of files' calls inside a transaction of the database it was opened
from: what they write commits with the transaction, and what they read
sees what it wrote.

### TxFiles.put

```rust
pub fn put(&self, path: &str, bytes: impl AsRef<[u8]>) -> Result<FileInfo>
```

Writes the file and replaces whatever the path held, once the
transaction commits.

### TxFiles.create

```rust
pub fn create(&self, path: &str, bytes: impl AsRef<[u8]>) -> Result<bool>
```

Writes the file only when the path holds none; says whether it did.

### TxFiles.delete

```rust
pub fn delete(&self, path: &str) -> Result<()>
```

Removes the file at the path; nothing to remove is not an error.

### TxFiles.head

```rust
pub fn head(&self, path: &str) -> Result<Option<FileInfo>>
```

What the file at the path carries, as the transaction sees it.

### TxFiles.key

```rust
pub fn key(&self, path: &str) -> FileCall<'a>
```

A call on one path inside the transaction, its options before its
verb: `tx.with(&files).key("a.md").content_type("text/markdown").put(&text)`.

## Upload

```rust
pub struct Upload { /* private fields */ }
```

A file written a piece at a time, as `Files::upload` begins it: bytes go
in with `write`, the file appears at `commit` and never before, and an
upload dropped without it leaves nothing.

### Upload.commit

```rust
pub fn commit(self) -> Result<FileInfo>
```

Publishes the file whole, replacing what the path held, and returns
it once it is on disk as the store's durability says.

### Upload.abort

```rust
pub fn abort(self)
```

Ends the upload and leaves nothing; dropping it does the same.

## aio::blobs::SetMaxSize

```rust
pub struct SetMaxSize { /* private fields */ }
```

A change of a set's ceiling, its condition before `SetMaxSize::apply`.

### aio::blobs::SetMaxSize.if_current

```rust
pub fn if_current(self, bytes: u64) -> Self
```

Changes the ceiling only while the set keeps `bytes`.

### aio::blobs::SetMaxSize.when_full

```rust
pub fn when_full(self, when_full: WhenFull) -> Self
```

What a write past the new ceiling does; what the set does now when
not given.

### aio::blobs::SetMaxSize.apply

```rust
pub fn apply(self) -> impl Future<Output = Result<()>> + Send + 'static
```

Writes the ceiling on the store's pool, nothing when the set keeps it.

## aio::blobs::All

```rust
pub struct All { /* private fields */ }
```

Every file of a folder, a page at a time, as `Files::all` walks them:
each page read on the store's pool, and the page under way stopped when
the stream is dropped. A page that fails is the last item.

## aio::blobs::List

```rust
pub struct List<'f> { /* private fields */ }
```

A page of a folder's files, its options before `List::page`, as
`crate::blobs::List` gathers them.

### aio::blobs::List.prefix

```rust
pub fn prefix(self, prefix: impl Into<String>) -> Self
```

Only the files whose path within the folder starts with this text:
`"photos/1"` finds `photos/10.jpg` too.

### aio::blobs::List.after

```rust
pub fn after(self, path: impl Into<String>) -> Self
```

The page that starts past this path, as a page's `next` says it.

### aio::blobs::List.limit

```rust
pub fn limit(self, files: usize) -> Self
```

### aio::blobs::List.page

```rust
pub fn page(self) -> impl Future<Output = Result<Page>> + Send + 'static
```

## aio::blobs::StoredFile

```rust
pub struct StoredFile { /* private fields */ }
```

A file and its bytes as an awaited `get` found it, which stay what they
were then through a replace, a delete or an expiry.

It is a `Stream` of its bytes, 256 KiB a piece, from where
`StoredFile::seek` put it: from the first byte it is checked against the
SHA-256 taken when it was written, its last piece held back until it
matches and a changed byte failing `corrupt`; from anywhere else it is a
range, and not checked. The stream ends after an error, until a seek.

A file on disk is read on the store's pool, a piece at a time, and once
the store closed its stream ends with `Closed`; one kept inline is read
in the poll, from memory.

### aio::blobs::StoredFile.info

```rust
pub fn info(&self) -> &FileInfo
```

### aio::blobs::StoredFile.read_all

```rust
pub fn read_all(&self) -> impl Future<Output = Result<Vec<u8>>> + Send + 'static
```

The whole file from its first byte, checked; it holds every byte in
memory, so a large file is read as a stream. It leaves where the
stream reads next as it was. Dropped, it stops between two pieces of
256 KiB.

### aio::blobs::StoredFile.read_at

```rust
pub fn read_at<'a>(
    &self,
    bytes: &'a mut [u8],
    offset: u64,
) -> impl Future<Output = Result<usize>> + Send + 'a
```

Reads at `offset` without moving where the stream reads next; like
any range it is not checked. A file on disk is read on the store's
pool into a buffer of its own and copied into `bytes` once it is
done, so that a future dropped mid-read leaves no thread writing into
memory the program has taken back.

### aio::blobs::StoredFile.seek

```rust
pub fn seek(&mut self, to: SeekFrom) -> Result<u64>
```

Moves where the stream reads next, as `std::io::Seek` does: a plain
call, which lets go of a piece read for the place it left.

## aio::blobs::Upload

```rust
pub struct Upload { /* private fields */ }
```

A file written a piece at a time, as an awaited `upload` begins it: bytes
go in with `write`, the file appears at `commit` and never before, and an
upload dropped without it leaves nothing.

### aio::blobs::Upload.write

```rust
pub async fn write(&mut self, bytes: &[u8]) -> Result<()>
```

Takes the whole piece into the upload: in memory, in the poll, while
the file may stay inline, then on the store's pool into its file of
`uploads/`. Once polled its piece is written whether or not anyone
still awaits it, and the next call waits for it. An error ends the
upload, which leaves nothing.

### aio::blobs::Upload.commit

```rust
pub async fn commit(self) -> Result<FileInfo>
```

Publishes the file whole, replacing what the path held, once it is on
disk as the store's durability says: submitted in the poll when its
bytes stayed inline, synced, renamed and committed on the store's pool
otherwise, and on a copy passed to its home from a thread of its own.
Once polled it commits whether or not anyone still awaits it; dropped
unpolled, the upload aborts.

### aio::blobs::Upload.abort

```rust
pub fn abort(self)
```

Ends the upload and leaves nothing; dropping it does the same. A file
of `uploads/` is removed on the store's pool, after the pieces still
being written.

## aio::blobs::FilesBuilder

```rust
pub struct FilesBuilder<'s> { /* private fields */ }
```

What opens a set of files, as `crate::blobs::FilesBuilder` does.

### aio::blobs::FilesBuilder.ttl

```rust
pub fn ttl(self, ttl: Duration) -> Self
```

The term of every file written without its own, counted from its
write; not a ceiling.

### aio::blobs::FilesBuilder.idle

```rust
pub fn idle(self, idle: Duration) -> Self
```

The term of every file written without its own, counted from its last
`get` or `head`, as `crate::blobs::FilesBuilder::idle` counts it.

### aio::blobs::FilesBuilder.max_file_size

```rust
pub fn max_file_size(self, bytes: u64) -> Self
```

The largest file a write may leave: one past it is refused and leaves
nothing, and nothing stored is ever removed to make room.

### aio::blobs::FilesBuilder.max_size

```rust
pub fn max_size(self, bytes: u64) -> Self
```

The most the set's files may hold together, kept in the set, as
`crate::blobs::FilesBuilder::max_size` keeps it.

### aio::blobs::FilesBuilder.when_full

```rust
pub fn when_full(self, when_full: WhenFull) -> Self
```

What a write past `max_size` does, kept with the ceiling.

### aio::blobs::FilesBuilder.open

```rust
pub fn open(self) -> Result<Files>
```

Opens the set, a plain call made once at start: a first open may
migrate blobs.db.

## aio::blobs::Files

```rust
pub struct Files { /* private fields */ }
```

A set of files by path, or a folder of it, whose calls are awaited: the
same set as `crate::blobs::Files`, and the same set when one is made
from the other.

### aio::blobs::Files.set_max_size

```rust
pub fn set_max_size(&self, bytes: u64) -> SetMaxSize
```

Changes the set's ceiling for every handle, as
`crate::blobs::Files::set_max_size` does; a lower one evicts at once.

### aio::blobs::Files.remove_max_size

```rust
pub fn remove_max_size(&self) -> SetMaxSize
```

Removes the set's ceiling.

### aio::blobs::Files.trim

```rust
pub fn trim(&self, to: u64) -> impl Future<Output = Result<Trim>> + Send + 'static
```

Removes files in the eviction's order until the set holds at most `to`
bytes, as `crate::blobs::Files::trim` does, a batch a commit.

### aio::blobs::Files.list

```rust
pub fn list(&self) -> List<'_>
```

A page of this folder's files and of the folders in it, in the byte
order of their paths, its options before `List::page`.

### aio::blobs::Files.all

```rust
pub fn all(&self) -> All
```

Every file of this folder and of the folders in it, a page at a time,
holding nothing between pages: a `Stream` whose pages are read on the
store's pool, each as its item is asked for.

### aio::blobs::Files.usage

```rust
pub fn usage(&self) -> impl Future<Output = Result<Usage>> + Send + 'static
```

The files of this folder and of the folders in it, and their bytes, as
they are now: a count, not a bound.

### aio::blobs::Files.clear

```rust
pub fn clear(&self) -> impl Future<Output = Result<()>> + Send + 'static
```

Removes this folder and every folder in it, at once for every reader,
however many files it holds. On the files themselves it empties them.

### aio::blobs::Files.blocking

```rust
pub fn blocking(&self) -> &Files
```

The same files, for their blocking calls.

### aio::blobs::Files.folder

```rust
pub fn folder(&self, segment: impl Key) -> Files
```

The folder in this one that `segment` names: `folder("users").folder(42)`
is `users/42`, and its files' paths are its segments and then theirs.

### aio::blobs::Files.key

```rust
pub fn key(&self, path: &str) -> FileCall<'_>
```

A call on one path, its options before its verb:
`files.key("a.md").if_match(&etag).put(text)`.

### aio::blobs::Files.put

```rust
pub fn put(
    &self,
    path: &str,
    bytes: impl AsRef<[u8]> + Send + 'static,
) -> impl Future<Output = Result<FileInfo>> + Send + 'static
```

Writes the file and replaces whatever the path held, as
`FileCall::put` does.

### aio::blobs::Files.create

```rust
pub fn create(
    &self,
    path: &str,
    bytes: impl AsRef<[u8]> + Send + 'static,
) -> impl Future<Output = Result<bool>> + Send + 'static
```

Writes the file only when the path holds none; says whether it did.

### aio::blobs::Files.upload

```rust
pub fn upload(&self, path: &str) -> impl Future<Output = Result<Upload>> + Send + 'static
```

A file written a piece at a time, which appears at its `commit`, as
`FileCall::upload` begins it.

### aio::blobs::Files.get

```rust
pub fn get(&self, path: &str) -> impl Future<Output = Result<Option<StoredFile>>> + Send + 'static
```

The file at the path and the bytes it reads, which stay what they were
now through a replace, a delete or an expiry; none when there is none.

Its row is read in the first poll when a reader is idle, and a file
kept inline is answered there; a file on disk opens on the store's
pool. Dropped, it stops, and a file it opened is closed.

### aio::blobs::Files.head

```rust
pub fn head(&self, path: &str) -> impl Future<Output = Result<Option<FileInfo>>> + Send + 'static
```

What the file at the path carries, none of its bytes.

### aio::blobs::Files.delete

```rust
pub fn delete(&self, path: &str) -> impl Future<Output = Result<()>> + Send + 'static
```

Removes the file at the path; nothing to remove is not an error.

### aio::blobs::Files.copy

```rust
pub fn copy(&self, from: &str, to: &str) -> impl Future<Output = Result<FileInfo>> + Send + 'static
```

Writes the file at `from` at `to` too, sharing its bytes, as
`crate::blobs::Files::copy` does: a file at `to` is a conflict.

### aio::blobs::Files.rename

```rust
pub fn rename(
    &self,
    from: &str,
    to: &str,
) -> impl Future<Output = Result<FileInfo>> + Send + 'static
```

Moves the file at `from` to `to` in one write, as `copy` writes it.

### aio::blobs::Files.expire

```rust
pub fn expire(
    &self,
    path: &str,
    after: Duration,
) -> impl Future<Output = Result<bool>> + Send + 'static
```

Makes the file at the path expire `after` from now; says whether there
was one.

## aio::blobs::FileCall

```rust
pub struct FileCall<'f> { /* private fields */ }
```

One path and the options of a call on it, which ends in its verb, as
`crate::blobs::FileCall` gathers them.

### aio::blobs::FileCall.content_type

```rust
pub fn content_type(self, content_type: impl Into<String>) -> Self
```

### aio::blobs::FileCall.meta

```rust
pub fn meta(self, name: impl Into<String>, value: impl Into<String>) -> Self
```

A string of the program's the file carries, such as the name it was
uploaded as.

### aio::blobs::FileCall.ttl

```rust
pub fn ttl(self, ttl: Duration) -> Self
```

The file's own term, from its write.

### aio::blobs::FileCall.if_match

```rust
pub fn if_match(self, etag: impl Into<String>) -> Self
```

Writes only over the file with this ETag: one that changed, expired or
went since is a conflict, and nothing is written.

### aio::blobs::FileCall.size

```rust
pub fn size(self, bytes: u64) -> Self
```

How long the body is; one longer or shorter is refused and leaves
nothing.

### aio::blobs::FileCall.timeout

```rust
pub fn timeout(self, timeout: Duration) -> Self
```

How long a `get_or_create`'s claim of the path is renewed while its
function runs: 10 minutes unless said.

### aio::blobs::FileCall.get_or_create

```rust
pub fn get_or_create<T, E, W, F>(
    self,
    work: W,
) -> impl Future<Output = Result<StoredFile, E>> + Send + use<T, E, W, F>
where
    T: Into<Fill> + Send,
    E: From<Error> + Display + Send + 'static,
    W: FnMut() -> F + Send,
    F: Future<Output = Result<T, E>> + Send,
```

The file at the path, or, on a miss, the one the future `work` gives
makes, as `crate::blobs::FileCall::get_or_create` makes it: the
calls of the path wait for one another's runs, the blocking ones
among them, and a call waits on no thread.

```rust
let pages = store.files("pages").open()?;
let page = pages.key("7f3a.pdf").content_type("application/pdf").get_or_create(|| convert("lecture")).await?;
let pdf = page.read_all().await?;
```

The claim and the write of what `work` gave run on the store's pool,
a reader's bytes and a file's read there. Dropped while it waits for
another run, the call leaves and that run goes on; dropped while
`work` runs, the claim is let go and nothing is written, and the next
call runs its own.

### aio::blobs::FileCall.put

```rust
pub fn put(
    self,
    bytes: impl AsRef<[u8]> + Send + 'static,
) -> impl Future<Output = Result<FileInfo>> + Send + 'static
```

Writes the file and replaces whatever the path held, at the moment its
future is ready. The bytes are owned, since past 16 KiB a thread of
the store's pool writes them to disk while the future may be gone.

### aio::blobs::FileCall.create

```rust
pub fn create(
    self,
    bytes: impl AsRef<[u8]> + Send + 'static,
) -> impl Future<Output = Result<bool>> + Send + 'static
```

Writes the file only when the path holds no file; says whether it did.

### aio::blobs::FileCall.upload

```rust
pub fn upload(self) -> impl Future<Output = Result<Upload>> + Send + 'static
```

A file written a piece at a time, which appears at its `commit`. It
waits on no thread for the memory an upload holds, and dropped while
it waits it leaves the line and holds nothing.

### aio::blobs::FileCall.delete

```rust
pub fn delete(self) -> impl Future<Output = Result<()>> + Send + 'static
```

### aio::blobs::FileCall.copy_from

```rust
pub fn copy_from(self, from: &str) -> impl Future<Output = Result<FileInfo>> + Send + 'static
```

Writes the file at `from` here too, sharing its bytes.

### aio::blobs::FileCall.rename_from

```rust
pub fn rename_from(self, from: &str) -> impl Future<Output = Result<FileInfo>> + Send + 'static
```

Moves the file at `from` here in one write.

<!-- Generated by task reference from crates/tinystore through rustdoc. Edit the doc comments there, not this file. -->
