# Blobs API for Python

Every public class, function and type of the Blobs engine in `tinystore`, generated from its source. The [Blobs guide](../../guides/blobs/README.md) explains how to use them, and the [Bun and Node](../bun/blobs.md), [Go](../go/blobs.md) and [Rust](../rust/blobs.md) pages list the same API.

## FileInfo

```python
@dataclass(frozen=True, slots=True)
class FileInfo:
    path: str  # its path within the folder it was asked from
    size: int
    etag: str  # the SHA-256 of its bytes in hex, quoted, as an HTTP ETag is
    content_type: str
    last_modified: datetime
    expires: datetime | None  # None for a file that does not expire
    meta: dict[str, str] = field(default_factory=dict[str, str])
```

What a file carries: what serving it needs.

## FilePage

```python
@dataclass(frozen=True, slots=True)
class FilePage:
    files: list[FileInfo]
    next: str | None  # None on the last page
```

A page of a folder's files in the byte order of their paths, and where the next starts.

## Usage

```python
@dataclass(frozen=True, slots=True)
class Usage:
    count: int
    size: int
```

The files of a folder and their bytes, as it is now.

## Files

```python
class Files:
    name: str
```

A named set of files by path, or a folder of it, whose calls are the folder's.

A path in a folder is the folder's segments and then its own.

### Files.folder

```python
def folder(*segments: str | int) -> Files: ...
```

The folder of these segments, one each: ``folder("users", 42)`` is users/42, never users/420.

### Files.put

```python
async def put(
    path: str,
    body: Body,
    *,
    content_type: str | None = None,
    meta: dict[str, str] | None = None,
    ttl: Duration | None = None,
    if_match: str | None = None,
    size: int | None = None,
) -> FileInfo: ...
```

Writes the file whole and replaces whatever the path held.

It returns once the file is on disk as the store's durability says.
Nobody sees half of it, while it is written or after a crash.

### Files.create

```python
async def create(
    path: str,
    body: Body,
    *,
    content_type: str | None = None,
    meta: dict[str, str] | None = None,
    ttl: Duration | None = None,
    size: int | None = None,
) -> bool: ...
```

Writes the file only where the path holds none, and says whether it did.

### Files.get

```python
async def get(path: str) -> StoredFile | None: ...
```

What the file carries, and its bytes; None when the path holds none.

A file up to about a megabyte comes with them, and a larger one's are
read when they are read, on its ETag, as S3's GET with If-Match reads
them.

### Files.head

```python
async def head(path: str) -> FileInfo | None: ...
```

What the file carries and none of its bytes; None when the path holds none.

### Files.delete

```python
async def delete(path: str, *, if_match: str | None = None) -> None: ...
```

Deletes the file, or with if_match only that version of it; no file there is not an error.

### Files.copy

```python
async def copy(source: str, to: str, *, if_match: str | None = None) -> FileInfo: ...
```

A copy at to that shares the bytes, which are not copied.

A file at to is ConflictError unless if_match names the version to
replace.

### Files.rename

```python
async def rename(source: str, to: str, *, if_match: str | None = None) -> FileInfo: ...
```

The file moved to to in one write.

A file there is ConflictError unless if_match names the version to
replace, and none at source NotFoundError.

### Files.expire

```python
async def expire(path: str, after: Duration | datetime) -> bool: ...
```

Sets when the file expires, a span from now or a time; says whether there was a file.

### Files.list

```python
async def list(
    *,
    prefix: str = '',
    after: str | None = None,
    limit: int | None = None,
) -> FilePage: ...
```

A page of the files in the folder and in the folders in it, in the byte order of their paths.

### Files.all

```python
async def all(*, prefix: str = '') -> AsyncIterator[FileInfo]: ...
```

Every file of the folder, a page at a time, holding nothing between pages.

### Files.usage

```python
async def usage() -> Usage: ...
```

The files in the folder and their bytes, as it is now: it counts, it does not limit.

### Files.clear

```python
async def clear() -> None: ...
```

Removes the folder's every file and every folder in it, at once for every reader.

### Files.upload

```python
def upload(
    path: str,
    *,
    content_type: str | None = None,
    meta: dict[str, str] | None = None,
    ttl: Duration | None = None,
    if_match: str | None = None,
    size: int | None = None,
) -> Upload: ...
```

A file written a piece at a time, which appears at commit and never before.

``async with files.upload(path) as upload``, and leaving the block
without commit aborts it.

## Upload

```python
class Upload:
    ...
```

A file written a piece at a time, as ``files.upload`` begins it.

Bytes go in with write, the file appears at commit and never before, and
an upload that ends otherwise leaves nothing.

### Upload.write

```python
async def write(data: bytes | str) -> None: ...
```

Takes bytes, text as UTF-8, waiting while the store catches up.

### Upload.commit

```python
async def commit() -> FileInfo: ...
```

Publishes the file whole, replacing what the path held as put does, once it is on disk.

### Upload.abort

```python
async def abort() -> None: ...
```

Ends the upload, which leaves nothing; after commit it does nothing.

## FileBytes

```python
class FileBytes:
    size: int
```

Bytes of a file, read when they are read: ``async for chunk in file``, or all of them with read().

### FileBytes.read

```python
async def read() -> bytes: ...
```

Every byte, held in memory: a large file is read a chunk at a time with async for.

### FileBytes.text

```python
async def text() -> str: ...
```

### FileBytes.json

```python
async def json() -> Any: ...
```

## StoredFile

```python
class StoredFile(FileBytes):
    info: FileInfo
    path: str
    etag: str
    content_type: str
    last_modified: datetime
    expires: datetime | None
    meta: dict[str, str]
```

A file as get found it: what it carries, and its bytes.

A whole read is checked against its SHA-256 and fails CorruptError
before its last bytes when they changed; slice reads a range, which is
not checked.

### StoredFile.slice

```python
def slice(start: int | None = None, end: int | None = None) -> FileBytes: ...
```

A range of its bytes, as Python slices them: slice(-500) is the last 500 bytes.

<!-- Generated by task reference from sdk/python/src/tinystore/blobs.py. Edit the doc comments there, not this file. -->
