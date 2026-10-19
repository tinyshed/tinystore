# Blobs API for Rust

Every public type, trait and function of the Blobs engine in `tinystore::blobs`, generated from its source. The [Blobs guide](../../chat/attachments.md) explains how to use them, and the [Bun and Node](../bun/blobs.md), [Python](../python/blobs.md) and [Go](../go/blobs.md) pages list the same API.

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

<!-- Generated by task reference from crates/tinystore through rustdoc. Edit the doc comments there, not this file. -->
