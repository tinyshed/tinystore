// Package blobs is the application's files, as plan/api/blobs.md has them:
// files by path in named sets, written whole and replaced whole. A file's
// bytes go in pieces within the credit of the side that takes them, and a
// whole read fails Corrupt before its last piece when its bytes changed.
//
//	avatars, err := blobs.OpenFiles(store, "avatars")
//	info, err := avatars.Put(ctx, id+".png", bytes.NewReader(png), blobs.ContentType("image/png"))
//	avatar, found, err := avatars.Get(ctx, id+".png")   // an io.ReadSeekCloser http.ServeContent serves
package blobs

import (
	"context"
	"io"
	"iter"
	"strconv"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// FileInfo is what a file carries: what serving it needs.
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

// Page is a page of a folder's files in the byte order of their paths, and
// where the next starts: "" on the last.
type Page struct {
	Files []FileInfo
	Next  string
}

// Usage is the files of a folder and their bytes, as it is now.
type Usage struct {
	Count, Size int64
}

func infoOf(info wire.BlobsInfo) FileInfo {
	out := FileInfo{
		Path: info.Path, Size: int64(info.Size), ETag: info.Etag, ContentType: info.ContentType,
		LastModified: core.Moment(info.LastModified), Expires: core.MomentOf(info.Expires),
	}
	if info.Meta != nil {
		out.Meta = *info.Meta
	}
	return out
}

// Option is how files open.
type Option interface{ open(*openOptions) }

type openOptions struct {
	ttl         *time.Duration
	maxFileSize *uint64
}

// WriteOption is what a write gives besides its bytes.
type WriteOption interface{ write(*wire.BlobsWrite) }

// MatchOption is the version a delete, a copy or a rename needs the file at.
type MatchOption interface{ match(**string) }

// ListOption is how a page of files reads.
type ListOption interface{ list(*wire.BlobsList) }

type ttl time.Duration

// TTL is how long a file lives from its write: every file's, or one write's own.
func TTL(d time.Duration) ttl { return ttl(d) }

func (t ttl) open(o *openOptions) { d := time.Duration(t); o.ttl = &d }

func (t ttl) write(w *wire.BlobsWrite) {
	if ms, err := core.Millis(time.Duration(t), "a file's ttl"); err == nil {
		w.Ttl = &ms
	}
}

type maxFileSize int64

// MaxFileSize is the largest file the set takes, in bytes: a larger one is a Limit error.
func MaxFileSize(bytes int64) maxFileSize { return maxFileSize(bytes) }

func (m maxFileSize) open(o *openOptions) { n := uint64(max(int64(m), 0)); o.maxFileSize = &n }

type contentType string

// ContentType is the file's content type, which serving it says.
func ContentType(t string) contentType { return contentType(t) }

func (c contentType) write(w *wire.BlobsWrite) { s := string(c); w.ContentType = &s }

type meta map[string]string

// Meta is what the program keeps beside the file, small and by name.
func Meta(m map[string]string) meta { return meta(m) }

func (m meta) write(w *wire.BlobsWrite) { v := map[string]string(m); w.Meta = &v }

type ifMatch string

// IfMatch writes or removes only while the file is at this ETag: tinystore.ErrConflict otherwise.
func IfMatch(etag string) ifMatch { return ifMatch(etag) }

func (i ifMatch) write(w *wire.BlobsWrite) { s := string(i); w.IfMatch = &s }
func (i ifMatch) match(m **string)         { s := string(i); *m = &s }

type size int64

// Size is the bytes a write will send, which a reader of unknown length says
// so that a file past the set's largest is refused at once.
func Size(n int64) size { return size(n) }

func (s size) write(w *wire.BlobsWrite) { n := uint64(max(int64(s), 0)); w.Size = &n }

type prefix string

// Prefix lists the files whose paths start with it.
func Prefix(p string) prefix { return prefix(p) }

func (p prefix) list(l *wire.BlobsList) { l.Prefix = string(p) }

type after string

// After starts a page after the page before's Next.
func After(next string) after { return after(next) }

func (a after) list(l *wire.BlobsList) {
	if a != "" {
		s := string(a)
		l.After = &s
	}
}

type limit int

// Limit is how many files a page holds.
func Limit(n int) limit { return limit(n) }

func (n limit) list(l *wire.BlobsList) { v := uint64(max(int(n), 0)); l.Limit = &v }

// Files is a named set of files by path, or a folder of it, whose calls are
// the folder's: a path in a folder is the folder's segments and then its own.
type Files struct {
	store  *core.Store
	name   string
	opener *core.Opener
	via    core.Via
	folder []string
}

// OpenFiles opens a named set of files on the store. It opens at its first
// call; its error is a name or an option it refuses.
//
//	reports, err := blobs.OpenFiles(store, "reports", blobs.TTL(24*time.Hour), blobs.MaxFileSize(2<<30))
func OpenFiles(home tinystore.Home, name string, opts ...Option) (*Files, error) {
	var o openOptions
	for _, opt := range opts {
		opt.open(&o)
	}
	if err := core.CheckName(name, "files"); err != nil {
		return nil, err
	}
	message := wire.BlobsOpen{Name: name, MaxFileSize: o.maxFileSize}
	if o.ttl != nil {
		ms, err := core.Millis(*o.ttl, "files "+name+": ttl")
		if err != nil {
			return nil, err
		}
		message.Ttl = &ms
	}
	of := home.CoreHome()
	return &Files{
		store: of.Store, name: name,
		opener: &core.Opener{Method: wire.MethodBlobsOpen, Body: wire.Encode(&message)},
		via:    core.ViaLink{Link: of.Store.Link},
	}, nil
}

// Name is the set's name.
func (f *Files) Name() string { return f.name }

// Folder is the folder of these segments, one each: Folder("users", 42) is
// users/42, never users/420.
func (f *Files) Folder(segments ...any) *Files {
	folder := append([]string{}, f.folder...)
	for _, segment := range segments {
		switch s := segment.(type) {
		case string:
			folder = append(folder, s)
		case int:
			folder = append(folder, strconv.Itoa(s))
		case int64:
			folder = append(folder, strconv.FormatInt(s, 10))
		default:
			text, _ := core.KeyText(segment)
			folder = append(folder, text)
		}
	}
	return &Files{f.store, f.name, f.opener, f.via, folder}
}

func (f *Files) call(ctx context.Context, method uint16, body func(handle uint64) []byte, idempotence core.Idempotence) ([]byte, error) {
	return f.via.Call(ctx, f.opener, method, body, idempotence)
}

// Put writes the file whole and replaces whatever the path held. It returns
// once the file is on disk as the store's durability says; nobody sees half
// of it, while it is written or after a crash.
func (f *Files) Put(ctx context.Context, path string, body io.Reader, opts ...WriteOption) (FileInfo, error) {
	info, _, err := f.write(ctx, path, body, opts, false)
	return info, err
}

// Create writes the file only where the path holds none, and says whether it did.
func (f *Files) Create(ctx context.Context, path string, body io.Reader, opts ...WriteOption) (bool, error) {
	_, written, err := f.write(ctx, path, body, opts, true)
	return written, err
}

// Get is the file at path, and whether there is one: what it carries, and
// its bytes, an io.ReadSeekCloser and an io.ReaderAt. A file up to about a
// megabyte comes with them, and a larger one's are read when they are read,
// on its ETag, as S3's GET with If-Match reads them. Close it once read.
func (f *Files) Get(ctx context.Context, path string) (*StoredFile, bool, error) {
	answer, err := f.call(ctx, wire.MethodBlobsGet, func(handle uint64) []byte {
		return wire.Encode(&wire.BlobsAt{Handle: handle, Folder: f.folder, Path: path})
	}, core.Reading)
	if err != nil {
		return nil, false, err
	}
	var got wire.BlobsGot
	if err := decode(answer, &got); err != nil || got.Info == nil {
		return nil, false, err
	}
	stored := &StoredFile{FileInfo: infoOf(*got.Info), files: f, ctx: context.WithoutCancel(ctx), path: path}
	if int64(len(got.Bytes)) == stored.Size {
		stored.whole = got.Bytes
	}
	return stored, true, nil
}

// Head is what the file at path carries and none of its bytes, and whether there is one.
func (f *Files) Head(ctx context.Context, path string) (FileInfo, bool, error) {
	answer, err := f.call(ctx, wire.MethodBlobsHead, func(handle uint64) []byte {
		return wire.Encode(&wire.BlobsAt{Handle: handle, Folder: f.folder, Path: path})
	}, core.Reading)
	if err != nil {
		return FileInfo{}, false, err
	}
	var head wire.BlobsHead
	if err := decode(answer, &head); err != nil || head.Info == nil {
		return FileInfo{}, false, err
	}
	return infoOf(*head.Info), true, nil
}

// Delete deletes the file, or with IfMatch only that version of it; no file
// there is not an error.
func (f *Files) Delete(ctx context.Context, path string, opts ...MatchOption) error {
	message := wire.BlobsDelete{Folder: f.folder, Path: path}
	for _, opt := range opts {
		opt.match(&message.IfMatch)
	}
	_, err := f.call(ctx, wire.MethodBlobsDelete, func(handle uint64) []byte {
		message.Handle = handle
		return wire.Encode(&message)
	}, core.Writing)
	return err
}

// Copy is a copy at to that shares the bytes, which are not copied. A file
// at to is a conflict unless IfMatch names the version to replace.
func (f *Files) Copy(ctx context.Context, from, to string, opts ...MatchOption) (FileInfo, error) {
	return f.move(ctx, wire.MethodBlobsCopy, from, to, opts)
}

// Rename is the file moved to to in one write. A file there is a conflict
// unless IfMatch names the version to replace, and none at from NotFound.
func (f *Files) Rename(ctx context.Context, from, to string, opts ...MatchOption) (FileInfo, error) {
	return f.move(ctx, wire.MethodBlobsRename, from, to, opts)
}

func (f *Files) move(ctx context.Context, method uint16, from, to string, opts []MatchOption) (FileInfo, error) {
	message := wire.BlobsMove{Folder: f.folder, From: from, To: to}
	for _, opt := range opts {
		opt.match(&message.IfMatch)
	}
	answer, err := f.call(ctx, method, func(handle uint64) []byte {
		message.Handle = handle
		return wire.Encode(&message)
	}, core.Writing)
	if err != nil {
		return FileInfo{}, err
	}
	var info wire.BlobsInfo
	err = decode(answer, &info)
	return infoOf(info), err
}

// Expire sets the file to expire d from now; a path with no file is NotFound.
func (f *Files) Expire(ctx context.Context, path string, d time.Duration) error {
	ms, err := core.Millis(d, "a file's expiry")
	if err != nil {
		return err
	}
	return f.expire(ctx, path, ms)
}

// ExpireAt sets the file to expire at a time; a path with no file is NotFound.
func (f *Files) ExpireAt(ctx context.Context, path string, at time.Time) error {
	return f.expire(ctx, path, uint64(max(time.Until(at).Milliseconds(), 0)))
}

func (f *Files) expire(ctx context.Context, path string, after uint64) error {
	answer, err := f.call(ctx, wire.MethodBlobsExpire, func(handle uint64) []byte {
		return wire.Encode(&wire.BlobsExpire{Handle: handle, Folder: f.folder, Path: path, After: after})
	}, core.Writing)
	if err != nil {
		return err
	}
	var found wire.BlobsFound
	if err := decode(answer, &found); err != nil {
		return err
	}
	if !found.Found {
		return core.Errorf(core.NotFound, "files %s: no file at %q to expire", f.name, path)
	}
	return nil
}

// List is a page of the files in the folder and in the folders in it, in the
// byte order of their paths.
func (f *Files) List(ctx context.Context, opts ...ListOption) (Page, error) {
	message := wire.BlobsList{Folder: f.folder}
	for _, opt := range opts {
		opt.list(&message)
	}
	answer, err := f.call(ctx, wire.MethodBlobsList, func(handle uint64) []byte {
		message.Handle = handle
		return wire.Encode(&message)
	}, core.Reading)
	if err != nil {
		return Page{}, err
	}
	var page wire.BlobsPage
	if err := decode(answer, &page); err != nil {
		return Page{}, err
	}
	out := Page{}
	for _, each := range page.Files {
		out.Files = append(out.Files, infoOf(each))
	}
	if page.Next != nil {
		out.Next = *page.Next
	}
	return out, nil
}

// All is every file of the folder, a page at a time, holding nothing between pages.
func (f *Files) All(ctx context.Context, opts ...ListOption) iter.Seq2[FileInfo, error] {
	return func(yield func(FileInfo, error) bool) {
		next := ""
		for {
			page, err := f.List(ctx, append(opts, After(next))...)
			if err != nil {
				yield(FileInfo{}, err)
				return
			}
			for _, file := range page.Files {
				if !yield(file, nil) {
					return
				}
			}
			if page.Next == "" {
				return
			}
			next = page.Next
		}
	}
}

// Usage is the files in the folder and their bytes, as it is now: it counts, it does not limit.
func (f *Files) Usage(ctx context.Context) (Usage, error) {
	answer, err := f.call(ctx, wire.MethodBlobsUsage, func(handle uint64) []byte {
		return wire.Encode(&wire.BlobsFolder{Handle: handle, Folder: f.folder})
	}, core.Reading)
	if err != nil {
		return Usage{}, err
	}
	var usage wire.BlobsUsage
	err = decode(answer, &usage)
	return Usage{int64(usage.Count), int64(usage.Size)}, err
}

// Clear removes the folder's every file and every folder in it, at once for every reader.
func (f *Files) Clear(ctx context.Context) error {
	_, err := f.call(ctx, wire.MethodBlobsClear, func(handle uint64) []byte {
		return wire.Encode(&wire.BlobsFolder{Handle: handle, Folder: f.folder})
	}, core.Writing)
	return err
}

func decode(answer []byte, m wire.Message) error {
	if err := wire.Decode(answer, m); err != nil {
		return core.Wrap(core.Protocol, err, "an answer that does not read")
	}
	return nil
}
