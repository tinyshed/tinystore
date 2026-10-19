# Blobs API for Go

Every public type, function and constant of the Blobs engine in `tinyshed.org/tinystore/blobs`, generated from its source. The [Blobs guide](../../start/why.md#the-engines) explains how to use them, and the [Bun and Node](../bun/blobs.md), [Python](../python/blobs.md) and [Rust](../rust/blobs.md) pages list the same API.

## After

```go
func After(next string) after
```

After starts a page after the page before's Next.

## ContentType

```go
func ContentType(t string) contentType
```

ContentType is the file's content type, which serving it says.

## IfMatch

```go
func IfMatch(etag string) ifMatch
```

IfMatch writes or removes only while the file is at this ETag: tinystore.ErrConflict otherwise.

## Limit

```go
func Limit(n int) limit
```

Limit is how many files a page holds.

## MaxFileSize

```go
func MaxFileSize(bytes int64) maxFileSize
```

MaxFileSize is the largest file the set takes, in bytes: a larger one is a Limit error.

## Meta

```go
func Meta(m map[string]string) meta
```

Meta is what the program keeps beside the file, small and by name.

## Prefix

```go
func Prefix(p string) prefix
```

Prefix lists the files whose paths start with it.

## Size

```go
func Size(n int64) size
```

Size is the bytes a write will send, which a reader of unknown length says so that a file past the set's largest is refused at once.

## TTL

```go
func TTL(d time.Duration) ttl
```

TTL is how long a file lives from its write: every file's, or one write's own.

## FileInfo

```go
type FileInfo struct {
	// Path is its path within the folder it was asked from.
	Path string
	Size int64
	// ETag is the SHA-256 of its bytes in hex, quoted, as an HTTP ETag is.
	ETag         string
	ContentType  string
	LastModified time.Time
	// Expires is the zero time for a file that does not expire.
	Expires time.Time
	Meta    map[string]string
}
```

FileInfo is what a file carries: what serving it needs.

## Files

```go
type Files struct {
	// contains filtered or unexported fields
}
```

Files is a named set of files by path, or a folder of it, whose calls are the folder's: a path in a folder is the folder's segments and then its own.

### OpenFiles

```go
func OpenFiles(home tinystore.Home, name string, opts ...Option) (*Files, error)
```

OpenFiles opens a named set of files on the store. It opens at its first call; its error is a name or an option it refuses.

	reports, err := blobs.OpenFiles(store, "reports", blobs.TTL(24*time.Hour), blobs.MaxFileSize(2<<30))

### Files.All

```go
func (f *Files) All(ctx context.Context, opts ...ListOption) iter.Seq2[FileInfo, error]
```

All is every file of the folder, a page at a time, holding nothing between pages.

### Files.Clear

```go
func (f *Files) Clear(ctx context.Context) error
```

Clear removes the folder's every file and every folder in it, at once for every reader.

### Files.Copy

```go
func (f *Files) Copy(ctx context.Context, from, to string, opts ...MatchOption) (FileInfo, error)
```

Copy is a copy at to that shares the bytes, which are not copied. A file at to is a conflict unless IfMatch names the version to replace.

### Files.Create

```go
func (f *Files) Create(ctx context.Context, path string, body io.Reader, opts ...WriteOption) (bool, error)
```

Create writes the file only where the path holds none, and says whether it did.

### Files.Delete

```go
func (f *Files) Delete(ctx context.Context, path string, opts ...MatchOption) error
```

Delete deletes the file, or with IfMatch only that version of it; no file there is not an error.

### Files.Expire

```go
func (f *Files) Expire(ctx context.Context, path string, d time.Duration) error
```

Expire sets the file to expire d from now; a path with no file is NotFound.

### Files.ExpireAt

```go
func (f *Files) ExpireAt(ctx context.Context, path string, at time.Time) error
```

ExpireAt sets the file to expire at a time; a path with no file is NotFound.

### Files.Folder

```go
func (f *Files) Folder(segments ...any) *Files
```

Folder is the folder of these segments, one each: Folder("users", 42) is users/42, never users/420.

### Files.Get

```go
func (f *Files) Get(ctx context.Context, path string) (*StoredFile, bool, error)
```

Get is the file at path, and whether there is one: what it carries, and its bytes, an io.ReadSeekCloser and an io.ReaderAt. A file up to about a megabyte comes with them, and a larger one's are read when they are read, on its ETag, as S3's GET with If-Match reads them. Close it once read.

### Files.Head

```go
func (f *Files) Head(ctx context.Context, path string) (FileInfo, bool, error)
```

Head is what the file at path carries and none of its bytes, and whether there is one.

### Files.List

```go
func (f *Files) List(ctx context.Context, opts ...ListOption) (Page, error)
```

List is a page of the files in the folder and in the folders in it, in the byte order of their paths.

### Files.Name

```go
func (f *Files) Name() string
```

Name is the set's name.

### Files.Put

```go
func (f *Files) Put(ctx context.Context, path string, body io.Reader, opts ...WriteOption) (FileInfo, error)
```

Put writes the file whole and replaces whatever the path held. It returns once the file is on disk as the store's durability says; nobody sees half of it, while it is written or after a crash.

### Files.Rename

```go
func (f *Files) Rename(ctx context.Context, from, to string, opts ...MatchOption) (FileInfo, error)
```

Rename is the file moved to to in one write. A file there is a conflict unless IfMatch names the version to replace, and none at from NotFound.

### Files.Upload

```go
func (f *Files) Upload(ctx context.Context, path string, opts ...WriteOption) (*Upload, error)
```

Upload begins a file written a piece at a time:

	upload, err := reports.Upload(ctx, "2026-10/report.csv", blobs.ContentType("text/csv"))
	defer upload.Abort() // does nothing after Commit
	_, err = io.Copy(upload, rows)
	info, err := upload.Commit(ctx)

### Files.Usage

```go
func (f *Files) Usage(ctx context.Context) (Usage, error)
```

Usage is the files in the folder and their bytes, as it is now: it counts, it does not limit.

## ListOption

```go
type ListOption interface {
	// contains filtered or unexported methods
}
```

ListOption is how a page of files reads.

## MatchOption

```go
type MatchOption interface {
	// contains filtered or unexported methods
}
```

MatchOption is the version a delete, a copy or a rename needs the file at.

## Option

```go
type Option interface {
	// contains filtered or unexported methods
}
```

Option is how files open.

## Page

```go
type Page struct {
	Files []FileInfo
	Next  string
}
```

Page is a page of a folder's files in the byte order of their paths, and where the next starts: "" on the last.

## StoredFile

```go
type StoredFile struct {
	FileInfo
	// contains filtered or unexported fields
}
```

StoredFile is a file as Get found it: what it carries, and its bytes, an io.ReadSeekCloser and an io.ReaderAt that http.ServeContent serves. A whole read is checked against its SHA-256 and fails Corrupt before its last bytes when they changed; a read after a Seek, and ReadAt, read a range, which is not checked.

### StoredFile.Bytes

```go
func (s *StoredFile) Bytes() ([]byte, error)
```

Bytes is every byte of the file, held in memory: a large file is read with Read.

### StoredFile.Close

```go
func (s *StoredFile) Close() error
```

Close lets go of a read under way.

### StoredFile.Read

```go
func (s *StoredFile) Read(p []byte) (int, error)
```

### StoredFile.ReadAt

```go
func (s *StoredFile) ReadAt(p []byte, off int64) (int, error)
```

ReadAt reads len(p) bytes at off, a range of their own, as io.ReaderAt does.

### StoredFile.Seek

```go
func (s *StoredFile) Seek(offset int64, whence int) (int64, error)
```

Seek moves where the next Read reads, as io.Seeker does.

## Upload

```go
type Upload struct {
	// contains filtered or unexported fields
}
```

Upload is a file written a piece at a time, an io.Writer: it appears at Commit and never before, and an upload that ends otherwise leaves nothing.

### Upload.Abort

```go
func (u *Upload) Abort() error
```

Abort ends the upload, which leaves nothing; after Commit it does nothing.

### Upload.Commit

```go
func (u *Upload) Commit(ctx context.Context) (FileInfo, error)
```

Commit publishes the file whole, replacing what the path held as Put does, once it is on disk.

### Upload.Write

```go
func (u *Upload) Write(p []byte) (int, error)
```

Write takes bytes, waiting while the store catches up.

## Usage

```go
type Usage struct {
	Count, Size int64
}
```

Usage is the files of a folder and their bytes, as it is now.

## WriteOption

```go
type WriteOption interface {
	// contains filtered or unexported methods
}
```

WriteOption is what a write gives besides its bytes.

<!-- Generated by task reference from sdk/go/blobs/. Edit the doc comments there, not this file. -->
