package blobs

import (
	"bytes"
	"context"
	"io"
	"os"
	"time"

	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// Fill is what a GetOrCreate's function gives: the file's bytes, whole, as a
// reader, or in a file of this machine, one of the three, and the type and
// the meta it is written with in place of the call's.
type Fill struct {
	Data []byte
	// Reader is read to its end as it is written, a piece at a time.
	Reader io.Reader
	// Path is a file of this machine, opened and read as it is written.
	Path        string
	ContentType string
	Meta        map[string]string
}

type timeout time.Duration

// Timeout is how long a GetOrCreate's claim of its path is renewed while its
// function runs: 10 minutes unless said. A function still running then loses
// the claim within 30 s, and another call may run its own.
func Timeout(d time.Duration) timeout { return timeout(d) }

// write keeps nothing in a write: a GetOrCreate reads it from its options.
func (timeout) write(*wire.BlobsWrite) {}

// GetOrCreate is the file at path, or, on a miss, the one fn makes, written
// with the call's ContentType, Meta and TTL unless the Fill says its own.
//
//	page, err := pages.GetOrCreate(ctx, hash+".pdf", func(ctx context.Context) (blobs.Fill, error) {
//		pdf, err := convert(ctx, doc)
//		return blobs.Fill{Data: pdf, ContentType: "application/pdf"}, err
//	})
//
// While one call runs fn, every other GetOrCreate of the path, in this
// process or another of the store's clients, waits for it and gets the file
// it wrote. The call claims the path for 30 s and the store renews the claim
// while fn runs, up to its Timeout; what fn gives lands only while the claim
// still holds the path and no file is there. A Put of the path meanwhile
// wins, and the calls get the put's file; a Delete ends the claim, what fn
// gave is dropped, and one of the calls runs it again, so fn may run more
// than once. An error of fn is the caller's own and the answer of every call
// that waited for it, a store error of its kind and any other Conflict
// with its text; the next call runs fn again. A connection lost while fn
// runs loses the claim, and the call asks again on the next connection.
func (f *Files) GetOrCreate(ctx context.Context, path string, fn func(ctx context.Context) (Fill, error), opts ...WriteOption) (*StoredFile, error) {
	if err := f.outside("a getOrCreate"); err != nil {
		return nil, err
	}
	claim := wire.BlobsClaim{Folder: f.folder, Path: path}
	for _, opt := range opts {
		if t, ok := opt.(timeout); ok {
			ms, err := core.Millis(time.Duration(t), "files "+f.name+": a getOrCreate's timeout")
			if err != nil {
				return nil, err
			}
			claim.Timeout = &ms
		}
	}
	for {
		file, again, err := f.getOrCreateOnce(ctx, claim, fn, opts)
		if err != nil || !again {
			return file, err
		}
	}
}

// getOrCreateOnce asks for the path once on a connection: the file there,
// or the run handed over and what it wrote. It says to ask again when the
// run's write was dropped, the file it wrote went, or the connection was lost.
func (f *Files) getOrCreateOnce(ctx context.Context, claim wire.BlobsClaim, fn func(ctx context.Context) (Fill, error), opts []WriteOption) (*StoredFile, bool, error) {
	c, err := f.store.Link.Connection(ctx)
	if err != nil {
		return nil, false, err
	}
	handle, err := core.HandleOn(ctx, c, f.opener)
	if lost, _ := core.IsConnectionLost(err); lost {
		return nil, true, nil
	}
	if err != nil {
		return nil, false, err
	}
	claim.Handle = handle
	stream, err := c.Session.Open(ctx, wire.MethodBlobsGetOrCreate, wire.Encode(&claim), false)
	if err != nil {
		if lost, _ := core.IsConnectionLost(err); lost {
			return nil, true, nil
		}
		return nil, false, err
	}
	first, err := stream.Next(ctx)
	if err != nil {
		stream.Cancel()
		if lost, _ := core.IsConnectionLost(err); lost {
			return nil, true, nil
		}
		return nil, false, err
	}
	if first.End {
		var filled wire.BlobsFilled
		if err := decode(first.Body, &filled); err != nil || filled.Info == nil {
			if err == nil {
				err = core.Errorf(core.Protocol, "files %s: a getOrCreate ended without its file", f.name)
			}
			return nil, false, err
		}
		return f.stored(ctx, claim.Path, *filled.Info, filled.Bytes), false, nil
	}
	landed, err := f.run(ctx, c, stream, claim, fn, opts)
	if lost, _ := core.IsConnectionLost(err); lost {
		return nil, true, nil
	}
	if err != nil || !landed {
		return nil, err == nil, err
	}
	file, found, err := f.Get(ctx, claim.Path)
	return file, err == nil && !found, err
}

// run runs fn for the run handed over on stream, writes what it gave on
// the stream's connection naming the stream, and ends the run with its last
// DATA: none, or the failure every waiting call then gets. It says whether
// the write landed.
func (f *Files) run(ctx context.Context, c *core.Connection, stream *core.Stream, claim wire.BlobsClaim, fn func(ctx context.Context) (Fill, error), opts []WriteOption) (bool, error) {
	fill, err := fn(ctx)
	if err == nil {
		var written *wire.BlobsInfo
		written, err = f.land(ctx, c, stream.ID, claim, fill, opts)
		if err == nil {
			return written != nil, ended(ctx, stream, nil)
		}
		if lost, _ := core.IsConnectionLost(err); lost {
			stream.Cancel()
			return false, err
		}
	}
	code := string(core.KindOf(err))
	if code == "" {
		code = string(core.Conflict)
	}
	// fn's own error, or the write's, is the one to give
	_ = ended(ctx, stream, &wire.Failure{Code: code, Message: err.Error()})
	return false, err
}

// land writes what fn gave on connection c, as a put when it fits a piece,
// else as an upload, naming the run's stream in fill.
func (f *Files) land(ctx context.Context, c *core.Connection, run uint32, claim wire.BlobsClaim, fill Fill, opts []WriteOption) (*wire.BlobsInfo, error) {
	stream := uint64(run)
	message := wire.BlobsWrite{Handle: claim.Handle, Folder: claim.Folder, Path: claim.Path, Fill: &stream}
	for _, opt := range opts {
		opt.write(&message)
	}
	if fill.ContentType != "" {
		message.ContentType = &fill.ContentType
	}
	if fill.Meta != nil {
		message.Meta = &fill.Meta
	}
	body, known := io.Reader(bytes.NewReader(fill.Data)), len(fill.Data)
	switch {
	case fill.Reader != nil:
		body, known = fill.Reader, -1
	case fill.Path != "":
		file, err := os.Open(fill.Path)
		if err != nil {
			return nil, core.Wrap(core.Invalid, err, "files %s: %s: the file a getOrCreate's function gave", f.name, claim.Path)
		}
		defer file.Close()
		body, known = file, -1
	}
	piece := core.Piece(c)
	if known >= 0 && known <= piece {
		return put(ctx, c, message, fill.Data)
	}
	upload, err := c.Session.Open(ctx, wire.MethodBlobsUpload, wire.Encode(&message), false)
	if err != nil {
		return nil, err
	}
	return uploaded(ctx, upload, body, piece, false)
}

// ended sends a run's last DATA and waits for the server's end; a stream
// that could not end so, its context gone, is cancelled, which lets the
// claim go rather than hold it until the connection ends.
func ended(ctx context.Context, stream *core.Stream, failure *wire.Failure) error {
	err := stream.Send(ctx, wire.Encode(&wire.BlobsFilled{Failure: failure}), true)
	if err == nil {
		_, err = stream.Next(ctx)
	}
	if err != nil {
		stream.Cancel()
	}
	return err
}

// stored is the file a getOrCreate's answer found, as Get makes it from a get's.
func (f *Files) stored(ctx context.Context, path string, info wire.BlobsInfo, whole []byte) *StoredFile {
	stored := &StoredFile{FileInfo: infoOf(info), ctx: context.WithoutCancel(ctx)}
	etag := stored.ETag
	stored.reader = func(ctx context.Context, offset, length int64) (*reading, error) {
		return f.read(ctx, path, etag, offset, length)
	}
	if int64(len(whole)) == stored.Size {
		stored.whole = whole
	}
	return stored
}
