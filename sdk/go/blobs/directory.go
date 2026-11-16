package blobs

// Directories, as docs/guides/blobs/directories.md has them: a folder of the
// store's machine under a name, which the store lists and reads and never
// writes. A scan writes down what appeared, changed or went; changes keep a
// copy in step from a cursor; a read comes from the disk, held to the
// version its Get opened, or in a growing directory to none.

import (
	"context"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// DirectoryOption is how a directory opens.
type DirectoryOption interface {
	directory(*wire.BlobsDirectory) error
}

type every time.Duration

// Every scans the folder on the store's background work, a period apart:
// minutes, not seconds.
func Every(d time.Duration) every { return every(d) }

func (e every) directory(m *wire.BlobsDirectory) error {
	ms, err := core.Millis(time.Duration(e), "directory "+m.Name+": every")
	m.Every = &ms
	return err
}

type ignore []string

// Ignore is names a scan passes over, a file's or a folder's, * for any run
// of a name's characters: a folder passed over is not entered.
func Ignore(patterns ...string) ignore { return ignore(patterns) }

func (i ignore) directory(m *wire.BlobsDirectory) error {
	m.Ignore = append(m.Ignore, i...)
	return nil
}

type growing bool

// Growing is for a folder whose files are written while they are read: a
// read holds to no version and goes up to the size the file has now.
func Growing() growing { return true }

func (g growing) directory(m *wire.BlobsDirectory) error {
	m.Growing = bool(g)
	return nil
}

// Change is what became of a file since a cursor.
type Change string

const (
	Added   Change = "added"
	Changed Change = "changed"
	Removed Change = "removed"
)

// Scan is what a scan found, and what the list holds once it ended.
type Scan struct {
	Added, Changed, Removed int64
	// Skipped is what it met and could not list: a link, a folder it cannot read, another disk.
	Skipped     int64
	Count, Size int64
}

// FileChange is a file's change since a cursor: its path in the directory,
// and what it carries now, nil once removed.
type FileChange struct {
	Path   string
	Change Change
	Info   *FileInfo
}

// Changes is a page of changes in the order the scans wrote them; Next is
// always there, with no items the cursor to ask from after the next scan.
type Changes struct {
	Items []FileChange
	Next  string
}

// ChangesOption is where a page of changes starts: After a page's Next.
type ChangesOption interface{ since(*wire.BlobsSince) }

func (a after) since(s *wire.BlobsSince) {
	if a != "" {
		text := string(a)
		s.After = &text
	}
}

// Directory is a directory, or a folder of it: Get, Head, List and Usage are
// a set of files' calls with a set of files' answers, answered from the list
// the last scan wrote but a Get's bytes, which come from the disk.
type Directory struct {
	store   *core.Store
	name    string
	opener  *core.Opener
	via     core.Via
	folder  []string
	growing bool
}

// OpenDirectory opens the directory name on the folder path of the store's
// machine; over a token, with path "", the server's own by its name. It
// opens at its first call; its error is a name or an option it refuses.
//
//	movies, err := blobs.OpenDirectory(store, "movies", "D:/Movies")
//	photos, err := blobs.OpenDirectory(store, "photos", "/mnt/photos", blobs.Every(15*time.Minute), blobs.Ignore("*.part", ".*"))
func OpenDirectory(home tinystore.Home, name, path string, opts ...DirectoryOption) (*Directory, error) {
	if err := core.CheckName(name, "directory"); err != nil {
		return nil, err
	}
	message := wire.BlobsDirectory{Name: name}
	if path != "" {
		message.Path = &path
	}
	for _, opt := range opts {
		if err := opt.directory(&message); err != nil {
			return nil, err
		}
	}
	of := home.CoreHome()
	if of.Database != nil {
		return nil, core.Errorf(core.Invalid, "directory %s: a directory is the store's, not a database's", name)
	}
	return &Directory{
		store: of.Store, name: name,
		opener:  &core.Opener{Method: wire.MethodBlobsDirectory, Body: wire.Encode(&message)},
		via:     core.ViaLink{Link: of.Store.Link},
		growing: message.Growing,
	}, nil
}

// Name is the directory's name.
func (d *Directory) Name() string { return d.name }

// Folder is the folder of these segments, one each, whose paths are relative to it.
func (d *Directory) Folder(segments ...any) *Directory {
	in := *d
	in.folder = folderOf(d.folder, segments)
	return &in
}

func (d *Directory) call(ctx context.Context, method uint16, body func(handle uint64) []byte, idempotence core.Idempotence) ([]byte, error) {
	return d.via.Call(ctx, d.opener, method, body, idempotence)
}

// Scan walks the folder and writes down what appeared, changed or went since the last scan.
func (d *Directory) Scan(ctx context.Context) (Scan, error) {
	answer, err := d.call(ctx, wire.MethodBlobsScan, func(handle uint64) []byte {
		return wire.Encode(&wire.BlobsFolder{Handle: handle, Folder: d.folder})
	}, core.Writing)
	if err != nil {
		return Scan{}, err
	}
	var scan wire.BlobsScanned
	if err := decode(answer, &scan); err != nil {
		return Scan{}, err
	}
	return Scan{
		Added: int64(scan.Added), Changed: int64(scan.Changed), Removed: int64(scan.Removed),
		Skipped: int64(scan.Skipped), Count: int64(scan.Count), Size: int64(scan.Size),
	}, nil
}

// Changes is the changes past a cursor, After a page's Next, or from the
// first file the directory listed.
func (d *Directory) Changes(ctx context.Context, opts ...ChangesOption) (Changes, error) {
	message := wire.BlobsSince{}
	for _, opt := range opts {
		opt.since(&message)
	}
	answer, err := d.call(ctx, wire.MethodBlobsChanges, func(handle uint64) []byte {
		message.Handle = handle
		return wire.Encode(&message)
	}, core.Reading)
	if err != nil {
		return Changes{}, err
	}
	var page wire.BlobsChanges
	if err := decode(answer, &page); err != nil {
		return Changes{}, err
	}
	changes := Changes{Items: make([]FileChange, 0, len(page.Items)), Next: page.Next}
	for _, item := range page.Items {
		change := FileChange{Path: item.Path, Change: Change(item.Change)}
		if item.Info != nil {
			info := infoOf(*item.Info)
			change.Info = &info
		}
		changes.Items = append(changes.Items, change)
	}
	return changes, nil
}

// Get is the file at path as a scan listed it and as the disk holds it now,
// and whether there is one: none when no scan listed it, or it is gone since.
// A read of a file that moved since fails Conflict, but in a growing
// directory, which reads up to the size now. Close it once read.
func (d *Directory) Get(ctx context.Context, path string) (*StoredFile, bool, error) {
	answer, err := d.call(ctx, wire.MethodBlobsGet, func(handle uint64) []byte {
		return wire.Encode(&wire.BlobsAt{Handle: handle, Folder: d.folder, Path: path})
	}, core.Reading)
	if err != nil {
		return nil, false, err
	}
	var got wire.BlobsGot
	if err := decode(answer, &got); err != nil || got.Info == nil {
		return nil, false, err
	}
	stored := &StoredFile{FileInfo: infoOf(*got.Info), ctx: context.WithoutCancel(ctx), growing: d.growing}
	message := wire.BlobsRead{Folder: d.folder, Path: path}
	if !d.growing {
		etag := stored.ETag
		message.IfMatch = &etag
		if int64(len(got.Bytes)) == stored.Size {
			stored.whole = got.Bytes
		}
	}
	stored.reader = func(ctx context.Context, offset, length int64) (*reading, error) {
		return readOn(ctx, d.store, d.opener, message, offset, length)
	}
	return stored, true, nil
}

// Head is what the last scan wrote down of the file at path, and whether it
// listed one; it reads no disk.
func (d *Directory) Head(ctx context.Context, path string) (FileInfo, bool, error) {
	answer, err := d.call(ctx, wire.MethodBlobsHead, func(handle uint64) []byte {
		return wire.Encode(&wire.BlobsAt{Handle: handle, Folder: d.folder, Path: path})
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

// List is a page of the folder's files in the byte order of their paths, as
// the last scan wrote them.
func (d *Directory) List(ctx context.Context, opts ...ListOption) (Page, error) {
	message := wire.BlobsList{Folder: d.folder}
	for _, opt := range opts {
		opt.list(&message)
	}
	answer, err := d.call(ctx, wire.MethodBlobsList, func(handle uint64) []byte {
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
	files := make([]FileInfo, 0, len(page.Files))
	for _, info := range page.Files {
		files = append(files, infoOf(info))
	}
	out := Page{Files: files}
	if page.Next != nil {
		out.Next = *page.Next
	}
	return out, nil
}

// Usage is the files the last scan listed in the folder, and their bytes.
func (d *Directory) Usage(ctx context.Context) (Usage, error) {
	answer, err := d.call(ctx, wire.MethodBlobsUsage, func(handle uint64) []byte {
		return wire.Encode(&wire.BlobsFolder{Handle: handle, Folder: d.folder})
	}, core.Reading)
	if err != nil {
		return Usage{}, err
	}
	var usage wire.BlobsUsage
	if err := decode(answer, &usage); err != nil {
		return Usage{}, err
	}
	return Usage{Count: int64(usage.Count), Size: int64(usage.Size)}, nil
}

// Forget removes every file of the list, each told Removed, and nothing on the disk.
func (d *Directory) Forget(ctx context.Context) error {
	_, err := d.call(ctx, wire.MethodBlobsForget, func(handle uint64) []byte {
		return wire.Encode(&wire.BlobsFolder{Handle: handle, Folder: d.folder})
	}, core.Writing)
	return err
}
