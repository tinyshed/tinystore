package blobs

import (
	"bytes"
	"context"
	"errors"
	"io"

	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// lengthy is a reader that knows how many bytes it holds: a bytes.Reader, a
// strings.Reader, a bytes.Buffer.
type lengthy interface{ Len() int }

// write writes a body: in one call when it fits a piece, else as an upload
// over a stream. It gives what it wrote, and false when a create found a file.
func (f *Files) write(ctx context.Context, path string, body io.Reader, opts []WriteOption, create bool) (FileInfo, bool, error) {
	message := wire.BlobsWrite{Folder: f.folder, Path: path, Create: create}
	for _, opt := range opts {
		opt.write(&message)
	}
	known := -1
	if l, ok := body.(lengthy); ok {
		known = l.Len()
		if message.Size == nil {
			n := uint64(known)
			message.Size = &n
		}
	}
	seeker, replayable := body.(io.Seeker)
	var start int64
	if replayable {
		var err error
		if start, err = seeker.Seek(0, io.SeekCurrent); err != nil {
			replayable = false
		}
	}
	var written *wire.BlobsInfo
	// a body sent in one call, read once: a call sent again on another
	// connection sends the same bytes, from a reader that cannot seek too
	var whole []byte
	tries := 0
	err := f.store.Link.Run(ctx, core.Writing, func(c *core.Connection) error {
		if tries++; tries > 1 && replayable && whole == nil {
			if _, err := seeker.Seek(start, io.SeekStart); err != nil {
				return err
			}
		}
		handle, err := core.HandleOn(ctx, c, f.opener)
		if err != nil {
			return err
		}
		m := message
		m.Handle = handle
		piece := core.Piece(c)
		if whole != nil || (known >= 0 && known <= piece) {
			if whole == nil {
				whole = make([]byte, known)
				if _, err := io.ReadFull(body, whole); err != nil {
					return core.Wrap(core.Invalid, err, "files %s: %s: its body", f.name, path)
				}
			}
			m.Bytes = whole
			answer, err := c.Session.Call(ctx, wire.MethodBlobsPut, wire.Encode(&m))
			if err != nil {
				return err
			}
			var w wire.BlobsWritten
			if err := decode(answer, &w); err != nil {
				return err
			}
			written = w.Info
			return nil
		}
		stream, err := c.Session.Open(ctx, wire.MethodBlobsUpload, wire.Encode(&m), false)
		if err != nil {
			return err
		}
		written, err = uploaded(ctx, stream, body, piece, replayable)
		return err
	})
	if err != nil || written == nil {
		return FileInfo{}, false, err
	}
	return infoOf(*written), true, nil
}

// uploaded sends an upload's pieces as DATA within the stream's credit, the
// last ending them, and waits for what the commit wrote. A connection lost
// before that last DATA left aborted the upload, which left nothing, and a
// body that reads again goes again on the next connection; lost after it,
// the commit's outcome is unknown.
func uploaded(ctx context.Context, stream *core.Stream, body io.Reader, piece int, replayable bool) (*wire.BlobsInfo, error) {
	buffer := make([]byte, piece)
	var last []byte
	ending := false
	fail := func(err error) (*wire.BlobsInfo, error) {
		if lost, _ := core.IsConnectionLost(err); lost {
			if !ending && replayable {
				return nil, core.Unsent(err)
			}
			stream.Cancel()
			return nil, lostAs(err, ending)
		}
		stream.Cancel()
		return nil, err
	}
	for {
		n, err := io.ReadFull(body, buffer)
		if n > 0 {
			if last != nil {
				if sendErr := stream.Send(ctx, wire.Encode(&wire.BlobsPiece{Bytes: last}), false); sendErr != nil {
					return fail(sendErr)
				}
			}
			last = append([]byte{}, buffer[:n]...)
		}
		if errors.Is(err, io.EOF) || errors.Is(err, io.ErrUnexpectedEOF) {
			break
		}
		if err != nil {
			stream.Cancel()
			return nil, err
		}
	}
	ending = true
	if err := stream.Send(ctx, wire.Encode(&wire.BlobsPiece{Bytes: last}), true); err != nil {
		return fail(err)
	}
	return committed(ctx, stream, fail)
}

// committed reads the RESPONSE that began an upload, then what its commit wrote.
func committed(ctx context.Context, stream *core.Stream, fail func(error) (*wire.BlobsInfo, error)) (*wire.BlobsInfo, error) {
	if _, err := stream.Next(ctx); err != nil {
		return fail(err)
	}
	event, err := stream.Next(ctx)
	if err != nil {
		return fail(err)
	}
	var w wire.BlobsWritten
	if err := decode(event.Body, &w); err != nil {
		return nil, err
	}
	return w.Info, nil
}

// lostAs is what a lost connection means for an upload: nothing left before
// its last DATA, unknown after.
func lostAs(err error, ending bool) error {
	if ending {
		return core.Wrap(core.OutcomeUnknown, err, "the connection was lost as the upload committed")
	}
	return core.Wrap(core.Closed, err, "the connection was lost during the upload, which left nothing")
}

// Upload is a file written a piece at a time, an io.Writer: it appears at
// Commit and never before, and an upload that ends otherwise leaves nothing.
type Upload struct {
	ctx    context.Context
	stream *core.Stream
	piece  int
	held   []byte
	done   bool
}

// Upload begins a file written a piece at a time:
//
//	upload, err := reports.Upload(ctx, "2026-10/report.csv", blobs.ContentType("text/csv"))
//	defer upload.Abort() // does nothing after Commit
//	_, err = io.Copy(upload, rows)
//	info, err := upload.Commit(ctx)
func (f *Files) Upload(ctx context.Context, path string, opts ...WriteOption) (*Upload, error) {
	message := wire.BlobsWrite{Folder: f.folder, Path: path}
	for _, opt := range opts {
		opt.write(&message)
	}
	c, err := f.store.Link.Connection(ctx)
	if err != nil {
		return nil, err
	}
	handle, err := core.HandleOn(ctx, c, f.opener)
	if err != nil {
		return nil, err
	}
	message.Handle = handle
	stream, err := c.Session.Open(ctx, wire.MethodBlobsUpload, wire.Encode(&message), false)
	if err != nil {
		return nil, err
	}
	return &Upload{ctx: ctx, stream: stream, piece: core.Piece(c)}, nil
}

// Write takes bytes, waiting while the store catches up.
func (u *Upload) Write(p []byte) (int, error) {
	if u.done {
		return 0, core.Errorf(core.Invalid, "the upload has ended")
	}
	u.held = append(u.held, p...)
	for len(u.held) >= u.piece {
		chunk := u.held[:u.piece]
		if err := u.stream.Send(u.ctx, wire.Encode(&wire.BlobsPiece{Bytes: chunk}), false); err != nil {
			if lost, _ := core.IsConnectionLost(err); lost {
				err = lostAs(err, false)
			}
			return 0, err
		}
		u.held = append([]byte{}, u.held[u.piece:]...)
	}
	return len(p), nil
}

// Commit publishes the file whole, replacing what the path held as Put does, once it is on disk.
func (u *Upload) Commit(ctx context.Context) (FileInfo, error) {
	if u.done {
		return FileInfo{}, core.Errorf(core.Invalid, "the upload has ended")
	}
	u.done = true
	fail := func(err error) (*wire.BlobsInfo, error) {
		if lost, _ := core.IsConnectionLost(err); lost {
			return nil, lostAs(err, true)
		}
		return nil, err
	}
	if err := u.stream.Send(ctx, wire.Encode(&wire.BlobsPiece{Bytes: u.held}), true); err != nil {
		// the last piece never left, so the upload is let go as Abort would
		u.stream.Cancel()
		_, err = fail(err)
		return FileInfo{}, err
	}
	written, err := committed(ctx, u.stream, fail)
	if err != nil || written == nil {
		return FileInfo{}, err
	}
	return infoOf(*written), nil
}

// Abort ends the upload, which leaves nothing; after Commit it does nothing.
func (u *Upload) Abort() error {
	if !u.done {
		u.done = true
		u.stream.Cancel()
	}
	return nil
}

// StoredFile is a file as Get found it: what it carries, and its bytes, an
// io.ReadSeekCloser and an io.ReaderAt that http.ServeContent serves. A
// whole read is checked against its SHA-256 and fails Corrupt before its
// last bytes when they changed; a read after a Seek, and ReadAt, read a
// range, which is not checked.
type StoredFile struct {
	FileInfo
	files  *Files
	ctx    context.Context
	path   string
	whole  []byte
	offset int64
	open   *reading
}

// reading is a read of a file's bytes on a stream, from where it began.
type reading struct {
	stream *core.Stream
	held   []byte
	ended  bool
}

func (s *StoredFile) Read(p []byte) (int, error) {
	if s.offset >= s.Size {
		return 0, io.EOF
	}
	if s.whole != nil {
		n := copy(p, s.whole[s.offset:])
		s.offset += int64(n)
		return n, nil
	}
	if s.open == nil {
		var err error
		if s.open, err = s.files.read(s.ctx, s.path, s.ETag, s.offset, -1); err != nil {
			return 0, err
		}
	}
	n, err := s.open.read(s.ctx, p)
	s.offset += int64(n)
	if errors.Is(err, io.EOF) && s.offset < s.Size {
		err = io.ErrUnexpectedEOF
	}
	return n, err
}

// Seek moves where the next Read reads, as io.Seeker does.
func (s *StoredFile) Seek(offset int64, whence int) (int64, error) {
	next := offset
	switch whence {
	case io.SeekCurrent:
		next += s.offset
	case io.SeekEnd:
		next += s.Size
	}
	if next < 0 {
		return s.offset, core.Errorf(core.Invalid, "a seek to %d, before the file's start", next)
	}
	if next != s.offset {
		s.closeRead()
		s.offset = next
	}
	return next, nil
}

// ReadAt reads len(p) bytes at off, a range of their own, as io.ReaderAt does.
func (s *StoredFile) ReadAt(p []byte, off int64) (int, error) {
	if off >= s.Size {
		return 0, io.EOF
	}
	if s.whole != nil {
		n := copy(p, s.whole[off:])
		if n < len(p) {
			return n, io.EOF
		}
		return n, nil
	}
	length := min(int64(len(p)), s.Size-off)
	r, err := s.files.read(s.ctx, s.path, s.ETag, off, length)
	if err != nil {
		return 0, err
	}
	defer r.stream.Cancel()
	n, err := io.ReadFull(readerOf{r, s.ctx}, p[:length])
	if err == nil && int64(n) < int64(len(p)) {
		err = io.EOF
	}
	return n, err
}

// Bytes is every byte of the file, held in memory: a large file is read with Read.
func (s *StoredFile) Bytes() ([]byte, error) {
	if s.whole != nil {
		return s.whole, nil
	}
	if _, err := s.Seek(0, io.SeekStart); err != nil {
		return nil, err
	}
	var all bytes.Buffer
	_, err := io.Copy(&all, s)
	return all.Bytes(), err
}

// Close lets go of a read under way.
func (s *StoredFile) Close() error {
	s.closeRead()
	return nil
}

func (s *StoredFile) closeRead() {
	if s.open != nil {
		s.open.stream.Cancel()
		s.open = nil
	}
}

type readerOf struct {
	r   *reading
	ctx context.Context
}

func (r readerOf) Read(p []byte) (int, error) { return r.r.read(r.ctx, p) }

// read opens a read of a file's bytes as S3's GET reads them with a range and
// If-Match: on the ETag a get gave, a conflict once the path holds another
// file or none; the whole file checked, a range not.
func (f *Files) read(ctx context.Context, path, etag string, offset, length int64) (*reading, error) {
	message := wire.BlobsRead{Folder: f.folder, Path: path, IfMatch: &etag}
	if offset > 0 || length >= 0 {
		o := uint64(offset)
		message.Offset = &o
	}
	if length >= 0 {
		l := uint64(length)
		message.Length = &l
	}
	var r *reading
	err := f.store.Link.Run(ctx, core.Reading, func(c *core.Connection) error {
		handle, err := core.HandleOn(ctx, c, f.opener)
		if err != nil {
			return err
		}
		message.Handle = handle
		stream, err := c.Session.Open(ctx, wire.MethodBlobsRead, wire.Encode(&message), true)
		if err != nil {
			return err
		}
		first, err := stream.Next(ctx)
		if err != nil {
			return err
		}
		// the RESPONSE's piece is no DATA, so it takes none of the credit
		var piece wire.BlobsPiece
		if err := decode(first.Body, &piece); err != nil {
			return err
		}
		r = &reading{stream: stream, held: piece.Bytes, ended: first.End}
		return nil
	})
	return r, err
}

func (r *reading) read(ctx context.Context, p []byte) (int, error) {
	for len(r.held) == 0 {
		if r.ended {
			return 0, io.EOF
		}
		event, err := r.stream.Next(ctx)
		if err != nil {
			return 0, err
		}
		if event.End {
			r.ended = true
			continue
		}
		r.stream.Consumed(len(event.Body))
		var piece wire.BlobsPiece
		if err := decode(event.Body, &piece); err != nil {
			return 0, err
		}
		r.held = piece.Bytes
	}
	n := copy(p, r.held)
	r.held = r.held[n:]
	return n, nil
}
