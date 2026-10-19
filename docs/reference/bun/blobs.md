# Blobs API for Bun and Node

Every public class, function and type of the Blobs engine in `@tinyshed/tinystore`, generated from its source. The [Blobs guide](../../chat/attachments.md) explains how to use them, and the [Python](../python/blobs.md), [Go](../go/blobs.md) and [Rust](../rust/blobs.md) pages list the same API.

## Size

```ts
type Size = number | `${number}${'KiB' | 'MiB' | 'GiB' | 'TiB'}`
```

A size in bytes: a number, or text in binary units, 64KiB or 2GiB; '2GB' does not compile.

## FilesOptions

```ts
interface FilesOptions {
    /** the term of every file written without its own; not a ceiling */
    ttl?: Duration | undefined
    /** the largest file a write may leave: past it, a write is refused and leaves nothing */
    maxFileSize?: Size | undefined
}
```

## FileInfo

```ts
interface FileInfo {
    /** its path within the folder it was asked from */
    path: string
    size: number
    /** the SHA-256 of its bytes in hex, quoted, as an HTTP ETag is */
    etag: string
    contentType: string
    lastModified: Date
    /** undefined for a file that does not expire */
    expires: Date | undefined
    meta: Record<string, string>
}
```

What a file carries: what serving it needs.

## FileOptions

```ts
interface FileOptions {
    contentType?: string | undefined
    meta?: Record<string, string> | undefined
    /** the file's own term, from this write */
    ttl?: Duration | undefined
    /** the ETag of the one file this write may replace: another is ConflictError */
    ifMatch?: string | undefined
    /** the body's length: one longer or shorter is refused and leaves nothing */
    size?: number | undefined
    signal?: AbortSignal | undefined
}
```

## Body

```ts
type Body = string | Uint8Array | ArrayBuffer | Blob | ReadableStream<Uint8Array> | AsyncIterable<Uint8Array>
```

What a write takes: text, bytes, a Blob or a file, or a stream of bytes.

## FileListOptions

```ts
interface FileListOptions {
    /** text within the folder: 'photos/1' finds photos/10.jpg too */
    prefix?: string | undefined
    /** a page's next */
    after?: string | undefined
    /** 1000 at most, and when absent */
    limit?: number | undefined
}
```

## FilePage

```ts
interface FilePage {
    files: FileInfo[]
    /** undefined on the last page */
    next: string | undefined
}
```

A page of a folder's files in the byte order of their paths, and where the next starts.

## Files

```ts
class Files {
    readonly name: string
}
```

A named set of files by path, or a folder of it, whose calls are the
folder's: a path in it is the folder's segments and then its own.

### Files.folder

```ts
folder(...segments: (string | number | bigint)[]): Files
```

The folder of these segments, one each: `folder('users', 42)` is users/42, never users/420.

### Files.put

```ts
put(path: string, body: Body, options?: FileOptions): Promise<FileInfo>
```

Writes the file whole and replaces whatever the path held, returning
once it is on disk as the store's durability says. Nobody sees half of
it, while it is written or after a crash.

### Files.create

```ts
create(path: string, body: Body, options?: Omit<FileOptions, 'ifMatch'>): Promise<boolean>
```

Writes the file only where the path holds none, and says whether it did.

### Files.get

```ts
get(path: string): Promise<StoredFile | undefined>
```

What the file carries, and its bytes: a file up to about a megabyte comes
with them, and a larger one's are read when they are read, on its ETag,
as S3's GET with If-Match reads them. Undefined when the path holds none.

### Files.head

```ts
head(path: string): Promise<FileInfo | undefined>
```

What the file carries and none of its bytes; undefined when the path holds none.

### Files.delete

```ts
delete(path: string, options?: {
        ifMatch?: string | undefined
    }): Promise<void>
```

Deletes the file, or with ifMatch only that version of it; no file there is not an error.

### Files.copy

```ts
copy(from: string, to: string, options?: {
        ifMatch?: string | undefined
    }): Promise<FileInfo>
```

A copy at `to` that shares the bytes, which are not copied. A file at
`to` is ConflictError unless ifMatch names the version to replace.

### Files.rename

```ts
rename(from: string, to: string, options?: {
        ifMatch?: string | undefined
    }): Promise<FileInfo>
```

The file moved to `to` in one write: a file there is ConflictError
unless ifMatch names the version to replace, and none at `from` NotFoundError.

### Files.expire

```ts
expire(path: string, after: Duration | Date): Promise<boolean>
```

Sets when the file expires, a span from now or a moment; says whether there was a file.

### Files.list

```ts
list(options?: FileListOptions): Promise<FilePage>
```

A page of the files in the folder and in the folders in it, in the byte order of their paths.

### Files.all

```ts
all(options?: {
        prefix?: string | undefined
    }): AsyncGenerator<FileInfo>
```

Every file of the folder, a page at a time, holding nothing between pages.

### Files.usage

```ts
usage(): Promise<{
        count: number
        size: number
    }>
```

The files in the folder and their bytes, as it is now: it counts, it does not limit.

### Files.clear

```ts
clear(): Promise<void>
```

Removes the folder's every file and every folder in it, at once for every reader.

### Files.upload

```ts
upload(path: string, options?: FileOptions): Promise<Upload>
```

Begins a file written a piece at a time, which appears at `commit` and
never before: `await using upload = await files.upload(path)`, and
leaving the block without commit aborts it.

## Upload

```ts
class Upload implements AsyncDisposable {}
```

A file written a piece at a time, as `files.upload` begins it: bytes go in
with `write`, the file appears at `commit` and never before, and an upload
that ends otherwise leaves nothing.

### Upload.write

```ts
write(bytes: Uint8Array | string): Promise<void>
```

Takes bytes, text as UTF-8, waiting while the server catches up.

### Upload.commit

```ts
commit(): Promise<FileInfo>
```

Publishes the file whole, replacing what the path held as `put` does,
once it is on disk as the store's durability says.

### Upload.abort

```ts
abort(): Promise<void>
```

Ends the upload, which leaves nothing; after `commit` it does nothing.

### Upload.[Symbol.asyncDispose]

```ts
[Symbol.asyncDispose](): Promise<void>
```

## FileBytes

```ts
class FileBytes {
    readonly size: number
}
```

Bytes of a file, read when they are read, as a Blob's are: `stream()`, or
all of them with `bytes()`, `text()`, `json()` or `arrayBuffer()`, which
hold them in memory. Each read is a read of its own.

### FileBytes.stream

```ts
stream(): ReadableStream<Uint8Array>
```

Its bytes as they come, the credit of each given back once it is taken.

### FileBytes.bytes

```ts
bytes(): Promise<Uint8Array<ArrayBuffer>>
```

### FileBytes.arrayBuffer

```ts
arrayBuffer(): Promise<ArrayBuffer>
```

### FileBytes.text

```ts
text(): Promise<string>
```

### FileBytes.json

```ts
json(): Promise<unknown>
```

## StoredFile

```ts
class StoredFile extends FileBytes implements FileInfo {
    readonly path: string
    readonly etag: string
    readonly contentType: string
    readonly lastModified: Date
    readonly expires: Date | undefined
    readonly meta: Record<string, string>
}
```

A file as `get` found it: what it carries, and its bytes. A whole read is
checked against its SHA-256 and fails CorruptError before its last bytes
when they changed; `slice` reads a range, which is not checked.

### StoredFile.slice

```ts
slice(start?: number, end?: number): FileBytes
```

A range of its bytes, as Blob.slice takes one: an end left out is the
file's, and a negative place counts back from it, so `slice(-500)` is the
last 500 bytes.

<!-- Generated by task reference from sdk/js/src/blobs.ts. Edit the doc comments there, not this file. -->
