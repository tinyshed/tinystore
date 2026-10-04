# Blobs API for Go

Every public type, function and constant of the Blobs engine in `github.com/tinyshed/tinystore/blobs`, generated from its source. The [Blobs guide](../../blobs/README.md) explains how to use them, and the [Bun and Node](../bun/blobs.md) and [Python](../python/blobs.md) pages list the same API.

## ErrOutcomeUnknown

```go
var ErrOutcomeUnknown = sqlite.ErrOutcomeUnknown
```

ErrOutcomeUnknown is a write whose group's commit failed: the object may or may not be there, and its caller should Stat it before writing again.

## Bucket

```go
type Bucket struct {
	// contains filtered or unexported fields
}
```

Bucket is a handle on the objects of one bucket, or of the folder of it that Of names; it may be used from any number of goroutines.

### OpenBucket

```go
func OpenBucket(ctx context.Context, objects *Store, name string, options ...BucketOption) (*Bucket, error)
```

OpenBucket opens the bucket name of blobs.db, creating it the first time. A bucket holds bytes of any kind, so it has no type; its options are the program's, and may change between runs.

### Bucket.All

```go
func (b *Bucket) All(ctx context.Context, query Query) iter.Seq2[Object, error]
```

All is every key Scan finds, a page of Scan at a time. Each page is its own snapshot and none is held between pages, so a slow loop keeps no reader open, and an object written or deleted during the walk may or may not be met. An error ends the walk as its last element.

### Bucket.Clear

```go
func (b *Bucket) Clear(ctx context.Context) error
```

Clear removes every object of this handle's folder and of the folders under it, at once. Up to 10,000 objects go in one transaction.

A larger folder is marked cleared at the file's revision, which hides its objects from every call the moment the mark commits; maintenance then deletes them 10,000 a transaction. An object written after the Clear is a new object and stays.

### Bucket.Copy

```go
func (b *Bucket) Copy(ctx context.Context, from, to string, options ...Option) (Object, error)
```

Copy writes the object under from again under to, sharing its bytes: no byte is read or written, whatever its size. The copy is a new version with the source's type and meta unless the call gives others, and with the expiry a Put would give it.

Onto its own key a Copy changes those and keeps the bytes. It takes the options Put takes but Size, its conditions for to. An absent source is tinystore.ErrConflict.

### Bucket.Create

```go
func (b *Bucket) Create(ctx context.Context, key string, options ...Option) (*Upload, error)
```

Create begins an upload of key: bytes go in with Write, and the object appears at Commit. It takes the options Put takes. A condition is checked now, so that a doomed upload of gigabytes stops before its first byte, and again in the commit, which decides.

### Bucket.Delete

```go
func (b *Bucket) Delete(ctx context.Context, key string, options ...Option) error
```

Delete removes key and its bytes, once no other key names them. An absent key is not an error, except to IfMatch, which deletes only the version with that ETag.

### Bucket.Move

```go
func (b *Bucket) Move(ctx context.Context, from, to string, options ...Option) (Object, error)
```

Move is Copy that removes from in the same commit, the bytes untouched.

### Bucket.Of

```go
func (b *Bucket) Of(owners ...any) *Bucket
```

Of is the folder of this handle that owners name, a segment each: a string or an integer, an integer by its decimal text, so that Of("users", 42) is the folder users/42/, which never meets users/4/. An owner holding / is tinystore.ErrInvalid, returned by every call of the handle. A folder exists while it holds objects; nothing creates or removes one.

### Bucket.Open

```go
func (b *Bucket) Open(ctx context.Context, key string) (*Reader, bool, error)
```

Open returns a reader of the object under key as it is at this moment, and whether a live object is there.

A reader of a file holds the file open and nothing of the engine's, so it reads on after its key is deleted, replaced or expired, and after the store closes, until its own Close.

An inline object is checked before its first byte. A file is read from its first byte to its last in order, and checked before its last bytes are handed over; a range is not checked.

### Bucket.Put

```go
func (b *Bucket) Put(ctx context.Context, key string, r io.Reader, options ...Option) (Object, error)
```

Put stores what r yields under key, and returns once the object is durable: its bytes synced and its row committed. Whatever the key held is replaced whole at that moment. A \*bytes.Reader, \*bytes.Buffer or \*strings.Reader says its own Size.

Memory does not follow the object's size: 16 KiB hold the bytes while they may stay inline, and past them the bytes go to a file as they arrive. A long stream may use 64 KiB when the store's memory is available immediately.

### Bucket.Scan

```go
func (b *Bucket) Scan(ctx context.Context, query Query) (Page, error)
```

Scan is a page of every key under this handle's folder, the keys of the folders under it included, from one snapshot, in the byte order of their paths: 10.jpg before 9.jpg. With Query.Prefix it is the keys that start with it. A page holds at most 1000 objects.

### Bucket.Stat

```go
func (b *Bucket) Stat(ctx context.Context, key string) (Object, bool, error)
```

Stat is the object under key without its bytes, and whether a live object is there.

### Bucket.Usage

```go
func (b *Bucket) Usage(ctx context.Context) (Usage, error)
```

Usage is the objects under this handle's folder and their bytes, the folders under it included, counted from their rows in one snapshot; research's design/blobs.md has what that costs. An expired object and one a Clear hid are not counted. It is a check and not a limit: two uploads that each pass it can pass a quota together.

## BucketOption

```go
type BucketOption func(*bucketSettings)
```

BucketOption changes how a bucket treats its objects; none changes what its rows mean, so a program may change them between runs.

### DefaultTTL

```go
func DefaultTTL(d time.Duration) BucketOption
```

DefaultTTL is the expiry a write gives an object when it names none, d from the write.

### MaxSize

```go
func MaxSize(n int64) BucketOption
```

MaxSize bounds the bucket's objects: the upload that passes n bytes stops with tinystore.ErrLimit and leaves nothing. There is no bound unless it says.

## KeyError

```go
type KeyError struct {
	Bucket, Path string
	Err          error
}
```

KeyError is a call refused because of one object: its bucket and its whole path, owners included. errors.Is finds the store's sentinel in it.

### KeyError.Error

```go
func (e *KeyError) Error() string
```

### KeyError.Unwrap

```go
func (e *KeyError) Unwrap() error
```

## Maintenance

```go
type Maintenance struct {
	Expired  int   // objects past their expiry, removed
	Cleared  int   // objects a marked Clear hid, removed
	Removed  int   // files of contents no key names, removed
	Aborted  int   // uploads whose context ended while no call held them
	Scrubbed int64 // bytes the scrub read
	Damaged  int   // contents the scrub found changed or missing
}
```

## Object

```go
type Object struct {
	// Key is the object's path under the handle that returned it.
	Key  string
	Size int64
	// ETag is quoted, as an HTTP header carries it, and derives from the
	// bytes, so two objects with the same bytes have one ETag. It is compared
	// with ETags and nothing else.
	ETag        string
	ContentType string
	// Modified is the store's clock at the commit of this version.
	Modified time.Time
	// Expires is zero for an object that does not expire.
	Expires time.Time
	Meta    map[string]string
}
```

Object is an object as the file knows it.

## Option

```go
type Option func(*callSettings)
```

Option changes one call. Put and Create take every option; Copy and Move all but Size; Delete only IfMatch. Any other is tinystore.ErrInvalid.

### ContentType

```go
func ContentType(t string) Option
```

ContentType is the object's type, kept as it is given, up to 256 bytes.

### ExpireAt

```go
func ExpireAt(t time.Time) Option
```

ExpireAt gives the object until t by the store's clock.

### IfMatch

```go
func IfMatch(etag string) Option
```

IfMatch writes or deletes only while the key holds a live object with this ETag, as an HTTP If-Match header spells it: a list of ETags, any of them, or \* for any live object. Any other version, and an absent key, is tinystore.ErrConflict.

### IfNoneMatch

```go
func IfNoneMatch() Option
```

IfNoneMatch writes only while the key holds no live object, so that an upload retried after a timeout adds nothing.

### Meta

```go
func Meta(key, value string) Option
```

Meta adds a string for serving the object, a file's original name or a width: a key is \[a-z0-9]\[a-z0-9\_-]{0,63}, and keys and values together are at most 2 KiB. A write that names Meta replaces all of it.

### Size

```go
func Size(n int64) Option
```

Size says the stream's length, so that the engine places the object before its first byte; a stream shorter or longer is tinystore.ErrInvalid and leaves nothing. A negative n says nothing, so Size(r.ContentLength) needs no condition around it.

### TTL

```go
func TTL(d time.Duration) Option
```

TTL gives the object d from its write.

## Options

```go
type Options struct {
	// KeepFree is the disk an upload leaves free, so that one upload cannot
	// take the space every engine's commits need. It is 1 GiB when zero, and
	// nothing is checked when it is negative.
	KeepFree int64
}
```

Options changes how the engine keeps its files.

## Page

```go
type Page struct {
	Objects []Object
	More    bool
	Next    Query
}
```

Page is one page of a Scan, from one snapshot. More says the limit ended it before the folder did, and Next is the query that reads on.

## Query

```go
type Query struct {
	// Prefix keeps only the keys that start with it.
	Prefix string
	// After keeps only the keys past it, when it is not empty.
	After string
	Limit int // objects a page returns: 100 when zero, at most 1000
}
```

Query asks Scan for a page of every key under a handle's folder, its sub-folders' included, in the byte order of their paths.

## Reader

```go
type Reader struct {
	Object
	// contains filtered or unexported fields
}
```

Reader is an object as Open found it: its fields, and its bytes through Read, ReadAt and Seek, so that http.ServeContent serves it with its ranges and conditions. A reader of a file holds the file open and nothing of the engine's; an inline object's reader holds its bytes. ReadAt may be called from several goroutines, Read and Seek from one.

### Reader.Close

```go
func (r *Reader) Close() error
```

Close closes the reader's file and gives back the memory an inline object's bytes held; a second Close does nothing.

### Reader.Read

```go
func (r *Reader) Read(p []byte) (int, error)
```

Read reads on from where the last Read or Seek left off. A file read from its first byte to its last in order is checked: the Read that would hand over the last bytes of a file that does not hash to the object's SHA-256 is tinystore.ErrCorrupt instead, so a copy of a changed object fails before its end.

### Reader.ReadAt

```go
func (r *Reader) ReadAt(p []byte, off int64) (int, error)
```

ReadAt reads len(p) bytes at off, and io.EOF where the object ends; like any range it is not checked. A file that ends before its object does is tinystore.ErrCorrupt.

### Reader.Seek

```go
func (r *Reader) Seek(offset int64, whence int) (int64, error)
```

Seek sets where the next Read begins; a position past the end reads io.EOF.

## Store

```go
type Store struct {
	// contains filtered or unexported fields
}
```

Store is the engine: blobs.db, and the files beside it in blobs/.

### Open

```go
func Open(ctx context.Context, store *tinystore.Store, options Options) (*Store, error)
```

Open opens blobs/ inside the store: blobs.db and the files beside it. It removes what the uploads of a process that died left, and the store closes it and, unless it is Manual, runs its maintenance every minute.

### Store.Close

```go
func (s *Store) Close(ctx context.Context) error
```

Close aborts the uploads that have not committed, lets the work in flight finish, settles the ids no upload holds any more and closes blobs.db; a reader of a file reads on until its own Close. Cancellation stops waiting, not the cleanup. The store calls it: an application closes the store instead.

### Store.Maintain

```go
func (s *Store) Maintain(ctx context.Context) (Maintenance, error)
```

Maintain removes expired objects and the objects marked Clears hid, 10,000 a transaction and at most ten transactions of each a call, and removes the files no key names any more.

It also aborts the uploads whose context has ended, removes what uploads left behind, moves the settled mark up, and scrubs a slice of the contents. The store calls it every minute unless it is Manual.

### Store.Snapshot

```go
func (s *Store) Snapshot(ctx context.Context, dir string) ([]tinystore.SnapshotFile, error)
```

Snapshot copies blobs.db into dir while the engine keeps working, and links there every file the copy names. A link copies no byte and keeps the bytes after the engine removes its own name, so a 17 GB film costs the snapshot a directory entry.

Collection waits meanwhile, so that no file the copy names goes before it is linked. A file missing is left out and logged, and one the system will not link is copied.

## Upload

```go
type Upload struct {
	// contains filtered or unexported fields
}
```

Upload is an object whose bytes arrive over time: Write takes them, Commit makes the object appear whole under its key, and Abort leaves nothing, so that defer upload.Abort() is always safe. An Upload is one goroutine's.

It lives as long as the context given to Create: a call after that context ended finds the upload aborted, and maintenance aborts one left without calls. The store's Close aborts it too. Used after either, or after Commit or Abort, it is the context's error or tinystore.ErrClosed.

### Upload.Abort

```go
func (u *Upload) Abort()
```

Abort ends the upload and leaves nothing; after Commit it does nothing.

### Upload.Commit

```go
func (u *Upload) Commit(ctx context.Context) (Object, error)
```

Commit makes the object appear under its key, whole, and returns it once it is durable: its file synced and renamed into objects/ with its directory synced, then its row committed beside the writes of other goroutines. A stream shorter than its Size is tinystore.ErrInvalid, and a condition that no longer holds tinystore.ErrConflict; either leaves nothing.

### Upload.Write

```go
func (u *Upload) Write(p []byte) (int, error)
```

Write takes p's bytes into the upload: the first 16 KiB stay in memory while the object may yet be inline, and past them the bytes go to its file as they arrive. An error ends the upload, which leaves nothing.

## Usage

```go
type Usage struct {
	Objects int64
	Bytes   int64
}
```

Usage is the objects under a folder and their bytes, counted from their rows: a Copy counts its bytes although none was copied.

<!-- Generated by task reference from blobs/. Edit the doc comments there, not this file. -->
