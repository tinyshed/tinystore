# Blobs API for Go

Every public type, function and constant of the Blobs engine in `tinyshed.org/tinystore/blobs`, generated from its source. The [Blobs guide](../../guides/blobs/README.md) explains how to use them, and the [Bun and Node](../bun/blobs.md), [Python](../python/blobs.md) and [Rust](../rust/blobs.md) pages list the same API.

## NoMaxSize

```go
const NoMaxSize int64 = 0
```

NoMaxSize is the size SetMaxSize removes the ceiling with.

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

## Every

```go
func Every(d time.Duration) every
```

Every scans the folder on the store's background work, a period apart: minutes, not seconds.

## Growing

```go
func Growing() growing
```

Growing is for a folder whose files are written while they are read: a read holds to no version and goes up to the size the file has now.

## Idle

```go
func Idle(d time.Duration) idle
```

Idle is how long a file lives from its last Get or Head; never beside TTL.

## IfCurrent

```go
func IfCurrent(bytes int64) ifCurrent
```

IfCurrent changes the ceiling only while the set keeps this one: another is tinystore.ErrConflict.

## IfMatch

```go
func IfMatch(etag string) ifMatch
```

IfMatch writes or removes only while the file is at this ETag: tinystore.ErrConflict otherwise.

## Ignore

```go
func Ignore(patterns ...string) ignore
```

Ignore is names a scan passes over, a file's or a folder's, \* for any run of a name's characters: a folder passed over is not entered.

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

## MaxSize

```go
func MaxSize(bytes int64) maxSize
```

MaxSize is the most the set's files hold together, in bytes, kept in the set: a write past it removes the files used least recently, and an open with another is tinystore.ErrConflict. SetMaxSize changes it.

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

## Timeout

```go
func Timeout(d time.Duration) timeout
```

Timeout is how long a GetOrCreate's claim of its path is renewed while its function runs: 10 minutes unless said. A function still running then loses the claim within 30 s, and another call may run its own.

## WhenFull

```go
func WhenFull(f Full) whenFull
```

WhenFull is what a write past MaxSize does, kept with the ceiling: Evict, or Refuse.

## CeilingOption

```go
type CeilingOption interface {
	// contains filtered or unexported methods
}
```

CeilingOption is a condition of SetMaxSize, or what the new ceiling does when full.

## Change

```go
type Change string
```

Change is what became of a file since a cursor.

### Added

```go
const (
	Added   Change = "added"
	Changed Change = "changed"
	Removed Change = "removed"
)
```

## Changes

```go
type Changes struct {
	Items []FileChange
	Next  string
}
```

Changes is a page of changes in the order the scans wrote them; Next is always there, with no items the cursor to ask from after the next scan.

## ChangesOption

```go
type ChangesOption interface {
	// contains filtered or unexported methods
}
```

ChangesOption is where a page of changes starts: After a page's Next.

## Directory

```go
type Directory struct {
	// contains filtered or unexported fields
}
```

Directory is a directory, or a folder of it: Get, Head, List and Usage are a set of files' calls with a set of files' answers, answered from the list the last scan wrote but a Get's bytes, which come from the disk.

### OpenDirectory

```go
func OpenDirectory(home tinystore.Home, name, path string, opts ...DirectoryOption) (*Directory, error)
```

OpenDirectory opens the directory name on the folder path of the store's machine; over a token, with path "", the server's own by its name. It opens at its first call; its error is a name or an option it refuses.

	movies, err := blobs.OpenDirectory(store, "movies", "D:/Movies")
	photos, err := blobs.OpenDirectory(store, "photos", "/mnt/photos", blobs.Every(15*time.Minute), blobs.Ignore("*.part", ".*"))

### Directory.Changes

```go
func (d *Directory) Changes(ctx context.Context, opts ...ChangesOption) (Changes, error)
```

Changes is the changes past a cursor, After a page's Next, or from the first file the directory listed.

### Directory.Folder

```go
func (d *Directory) Folder(segments ...any) *Directory
```

Folder is the folder of these segments, one each, whose paths are relative to it.

### Directory.Forget

```go
func (d *Directory) Forget(ctx context.Context) error
```

Forget removes every file of the list, each told Removed, and nothing on the disk.

### Directory.Get

```go
func (d *Directory) Get(ctx context.Context, path string) (*StoredFile, bool, error)
```

Get is the file at path as a scan listed it and as the disk holds it now, and whether there is one: none when no scan listed it, or it is gone since. A read of a file that moved since fails Conflict, but in a growing directory, which reads up to the size now. Close it once read.

### Directory.Head

```go
func (d *Directory) Head(ctx context.Context, path string) (FileInfo, bool, error)
```

Head is what the last scan wrote down of the file at path, and whether it listed one; it reads no disk.

### Directory.List

```go
func (d *Directory) List(ctx context.Context, opts ...ListOption) (Page, error)
```

List is a page of the folder's files in the byte order of their paths, as the last scan wrote them.

### Directory.Name

```go
func (d *Directory) Name() string
```

Name is the directory's name.

### Directory.Scan

```go
func (d *Directory) Scan(ctx context.Context) (Scan, error)
```

Scan walks the folder and writes down what appeared, changed or went since the last scan.

### Directory.Usage

```go
func (d *Directory) Usage(ctx context.Context) (Usage, error)
```

Usage is the files the last scan listed in the folder, and their bytes.

## DirectoryOption

```go
type DirectoryOption interface {
	// contains filtered or unexported methods
}
```

DirectoryOption is how a directory opens.

## FileChange

```go
type FileChange struct {
	Path   string
	Change Change
	Info   *FileInfo
}
```

FileChange is a file's change since a cursor: its path in the directory, and what it carries now, nil once removed.

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

Files is a named set of files by path, or a folder of it, whose calls are the folder's: a path in a folder is the folder's segments and then its own. Inside a database's transaction, from In, its writes go through it.

### OpenFiles

```go
func OpenFiles(home tinystore.Home, name string, opts ...Option) (*Files, error)
```

OpenFiles opens a named set of files on the store, or on a database whose file then keeps it: its writes commit with the database's rows in a transaction, through In. It opens at its first call; its error is a name or an option it refuses.

	reports, err := blobs.OpenFiles(store, "reports", blobs.TTL(24*time.Hour), blobs.MaxFileSize(2<<30))
	packages, err := blobs.OpenFiles(db, "packages")

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

### Files.GetOrCreate

```go
func (f *Files) GetOrCreate(ctx context.Context, path string, fn func(ctx context.Context) (Fill, error), opts ...WriteOption) (*StoredFile, error)
```

GetOrCreate is the file at path, or, on a miss, the one fn makes, written with the call's ContentType, Meta and TTL unless the Fill says its own.

	page, err := pages.GetOrCreate(ctx, hash+".pdf", func(ctx context.Context) (blobs.Fill, error) {
		pdf, err := convert(ctx, doc)
		return blobs.Fill{Data: pdf, ContentType: "application/pdf"}, err
	})

While one call runs fn, every other GetOrCreate of the path, in this process or another of the store's clients, waits for it and gets the file it wrote. The call claims the path for 30 s and the store renews the claim while fn runs, up to its Timeout; what fn gives lands only while the claim still holds the path and no file is there. A Put of the path meanwhile wins, and the calls get the put's file; a Delete ends the claim, what fn gave is dropped, and one of the calls runs it again, so fn may run more than once. An error of fn is the caller's own and the answer of every call that waited for it, a store error of its kind and any other Conflict with its text; the next call runs fn again. A connection lost while fn runs loses the claim, and the call asks again on the next connection.

### Files.Head

```go
func (f *Files) Head(ctx context.Context, path string) (FileInfo, bool, error)
```

Head is what the file at path carries and none of its bytes, and whether there is one.

### Files.In

```go
func (f *Files) In(tx tinystore.InTx) *Files
```

In is the files inside a database's transaction: their writes, Put, Create, Delete, Copy and Rename, commit with its rows, and Head sees what it wrote. Files kept elsewhere are refused at their first call.

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

### Files.SetMaxSize

```go
func (f *Files) SetMaxSize(ctx context.Context, bytes int64, opts ...CeilingOption) error
```

SetMaxSize changes the set's ceiling for every handle, NoMaxSize removing it; a lower one evicts at once. One the set keeps already writes nothing.

	err := pages.SetMaxSize(ctx, 1<<30, blobs.IfCurrent(512<<20))

### Files.Trim

```go
func (f *Files) Trim(ctx context.Context, to int64) (Trim, error)
```

Trim removes files in the eviction's order until the set holds at most \`to\` bytes, and leaves its ceiling where it was; a set whose ceiling refuses is tinystore.ErrInvalid.

	trimmed, err := pages.Trim(ctx, 300<<20) // blobs.Trim{Evictions, Freed}

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

## Fill

```go
type Fill struct {
	Data []byte
	// Reader is read to its end as it is written, a piece at a time.
	Reader io.Reader
	// Path is a file of this machine, opened and read as it is written.
	Path        string
	ContentType string
	Meta        map[string]string
}
```

Fill is what a GetOrCreate's function gives: the file's bytes, whole, as a reader, or in a file of this machine, one of the three, and the type and the meta it is written with in place of the call's.

## Full

```go
type Full string
```

Full is what a write past a set's MaxSize does.

### Evict

```go
const (
	// Evict removes the files used least recently until the set fits: a cache.
	Evict Full = "evict"
	// Refuse fails the write tinystore.ErrLimit and removes nothing: a quota of files that must stay.
	Refuse Full = "refuse"
)
```

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

## Scan

```go
type Scan struct {
	Added, Changed, Removed int64
	// Skipped is what it met and could not list: a link, a folder it cannot read, another disk.
	Skipped     int64
	Count, Size int64
}
```

Scan is what a scan found, and what the list holds once it ended.

## StoredFile

```go
type StoredFile struct {
	FileInfo
	// contains filtered or unexported fields
}
```

StoredFile is a file as Get found it: what it carries, and its bytes, an io.ReadSeekCloser and an io.ReaderAt that http.ServeContent serves. A whole read is checked against its SHA-256 and fails Corrupt before its last bytes when they changed; a read after a Seek, and ReadAt, read a range, which is not checked.

A directory's file has no hash: a read holds to the version Get opened and fails Conflict once the file moved. A growing directory's holds to none, reads up to the size the file has as it reads, and each piece read in order brings that size and time into Size and LastModified.

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

## Trim

```go
type Trim struct {
	Evictions, Freed int64
}
```

Trim is what a trim removed: its files, and their sizes as Usage counts them.

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
	// MaxSize is the set's ceiling, 0 when it keeps none.
	MaxSize int64
	// Evictions is the files the ceiling removed since the store opened.
	Evictions int64
}
```

Usage is the files of a folder and their bytes, as it is now, and the set's ceiling and what it evicted since the store opened.

## WriteOption

```go
type WriteOption interface {
	// contains filtered or unexported methods
}
```

WriteOption is what a write gives besides its bytes.

<!-- Generated by task reference from sdk/go/blobs/. Edit the doc comments there, not this file. -->
