# Blobs API for Rust

Every public type, trait and function of the Blobs engine in `tinystore::blobs` and `tinystore::aio::blobs`, generated from its source. The [Blobs guide](../../guides/blobs/README.md) explains how to use them, and the [Bun and Node](../bun/blobs.md), [Python](../python/blobs.md) and [Go](../go/blobs.md) pages list the same API.

## Store

```rust
impl Store
```

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

### Files.head

```rust
pub fn head(&self, path: &str) -> Result<Option<FileInfo>>
```

What the file at the path carries, none of its bytes.

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
they are now: a count, not a bound.

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

What opens a set of files: its name, and the term and the bound of the
files written in it.

### FilesBuilder.ttl

```rust
pub fn ttl(self, ttl: Duration) -> Self
```

The term of every file written without its own, counted from its
write; not a ceiling.

### FilesBuilder.max_file_size

```rust
pub fn max_file_size(self, bytes: u64) -> Self
```

The largest file a write may leave: one past it is refused and leaves
nothing, and nothing stored is ever removed to make room.

### FilesBuilder.open

```rust
pub fn open(self) -> Result<Files>
```

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
}
```

What a folder holds now: its files and their bytes.

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

### aio::blobs::FilesBuilder.max_file_size

```rust
pub fn max_file_size(self, bytes: u64) -> Self
```

The largest file a write may leave: one past it is refused and leaves
nothing, and nothing stored is ever removed to make room.

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
