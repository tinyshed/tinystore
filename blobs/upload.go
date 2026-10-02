package blobs

import (
	"bytes"
	"context"
	"crypto/sha256"
	"errors"
	"fmt"
	"hash"
	"io"
	"os"
	"strings"
	"sync"

	"github.com/tinyshed/tinystore"
)

// Upload is an object whose bytes arrive over time: Write takes them, Commit
// makes the object appear whole under its key, and Abort leaves nothing, so
// that defer upload.Abort() is always safe. An Upload is one goroutine's.
//
// It lives as long as the context given to Create: a call after that context
// ended finds the upload aborted, and maintenance aborts one left without
// calls. The store's Close aborts it too. Used after either, or after Commit or
// Abort, it is the context's error or tinystore.ErrClosed.
type Upload struct {
	ctx         context.Context
	bucket      *Bucket
	call        call
	mu          sync.Mutex
	buffer      []byte // the bytes while they may stay inline, then what carries a Put's bytes to the file
	bufferTried bool
	probe       [1]byte  // the byte a full buffer reads to learn whether more follow
	at          lying    // where the bytes are
	file        *os.File // the file in uploads/, open while bytes go to it
	id          int64    // the content's id, once it has one
	hash        hash.Hash
	written     int64
	synced      int64 // bytes the file held at its last sync
	checked     int64 // bytes it held when the disk's free space was last checked
	ended       error // why the upload ended; nil while it lives
	reserved    *tinystore.Reservation
	leave       func() // gives back its slot and its place in the store
}

type lying int

const (
	inMemory  lying = iota // the buffer
	inUploads              // a file of uploads/, which no reader looks at
	inObjects              // renamed into objects/, before or after its commit
)

// Create begins an upload of key: bytes go in with Write, and the object
// appears at Commit. It takes the options Put takes. A condition is checked
// now, so that a doomed upload of gigabytes stops before its first byte, and
// again in the commit, which decides.
func (b *Bucket) Create(ctx context.Context, key string, options ...Option) (*Upload, error) {
	c, err := b.begin(key, "Create", options, putTakes)
	if err != nil {
		return nil, b.fail(c, err)
	}

	upload, err := b.startUpload(ctx, c)
	if err != nil {
		return nil, b.fail(c, err)
	}
	return upload, nil
}

// Put stores what r yields under key, and returns once the object is durable:
// its bytes synced and its row committed. Whatever the key held is replaced
// whole at that moment. A *bytes.Reader, *bytes.Buffer or *strings.Reader says
// its own Size.
//
// Memory does not follow the object's size: 16 KiB hold the bytes while they
// may stay inline, and past them the bytes go to a file as they arrive. A long
// stream may use 64 KiB when the store's memory is available immediately.
func (b *Bucket) Put(ctx context.Context, key string, r io.Reader, options ...Option) (Object, error) {
	c, err := b.begin(key, "Put", withLength(r, options), putTakes)
	if err != nil {
		return Object{}, b.fail(c, err)
	}

	upload, err := b.startUpload(ctx, c)
	if err != nil {
		return Object{}, b.fail(c, err)
	}
	defer upload.Abort()

	if err = upload.readFrom(r); err != nil {
		return Object{}, b.fail(c, err)
	}
	return upload.Commit(ctx)
}

// withLength says the length of a reader that knows it, before the options
// given, so that a Size among them wins
func withLength(r io.Reader, options []Option) []Option {
	length := -1
	switch r := r.(type) {
	case *bytes.Reader:
		length = r.Len()
	case *bytes.Buffer:
		length = r.Len()
	case *strings.Reader:
		length = r.Len()
	}
	if length < 0 {
		return options
	}
	return append([]Option{Size(int64(length))}, options...)
}

// startUpload admits an upload: one of the upload slots, the store's memory for
// its buffer, its bounds and condition checked. From then on the store's Close
// aborts it, and so does maintenance once ctx has ended.
func (b *Bucket) startUpload(ctx context.Context, c call) (*Upload, error) {
	leave, err := b.store.admitUpload(ctx)
	if err != nil {
		return nil, err
	}
	reserved, err := b.store.reserve(ctx, inlineSize)
	if err != nil {
		leave()
		return nil, err
	}
	u := &Upload{
		ctx: ctx, bucket: b, call: c, buffer: make([]byte, 0, inlineSize), hash: sha256.New(),
		reserved: reserved, leave: leave,
	}
	if err = u.checkStart(); err != nil {
		u.release(err)
		return nil, err
	}

	if !b.store.live.add(u) {
		u.release(errClosed)
		return nil, errClosed
	}
	return u, nil
}

// checkStart refuses an upload whose declared size passes its bounds or the
// disk's free space, or whose condition the key already fails
func (u *Upload) checkStart() error {
	declared, b := u.call.settings.size, u.bucket
	if b.maxSize > 0 && declared > b.maxSize {
		return &tinystore.LimitError{
			Name: "bytes of an object, the bucket's MaxSize", Wanted: declared,
			Bound: b.maxSize,
		}
	}
	if declared > inlineSize {
		if err := b.store.checkFree(declared); err != nil {
			return err
		}
	}
	if u.call.settings.ifMatch == "" && !u.call.settings.ifNoneMatch {
		return nil
	}
	existing, err := b.current(u.ctx, u.call)
	if err != nil {
		return err
	}
	return u.call.settings.check(existing, b.store.clock())
}

// Write takes p's bytes into the upload: the first 16 KiB stay in memory while
// the object may yet be inline, and past them the bytes go to its file as they
// arrive. An error ends the upload, which leaves nothing.
func (u *Upload) Write(p []byte) (int, error) {
	u.mu.Lock()
	defer u.mu.Unlock()
	if err := u.alive(); err != nil {
		return 0, err
	}
	if err := u.take(p); err != nil {
		u.end(err)
		return 0, u.bucket.fail(u.call, err)
	}
	return len(p), nil
}

// alive is why the upload cannot go on: it has ended, or its context has,
// which ends it now
func (u *Upload) alive() error {
	if u.ended == nil && u.ctx.Err() != nil {
		u.end(context.Cause(u.ctx))
	}
	return u.ended
}

// readFrom takes r's bytes into the upload through its own buffer. While they
// may stay inline they gather in its free end, and a full buffer reads one byte
// to learn whether more follow; once they go to the file the buffer carries
// them there.
//
// The lock is taken between reads rather than across them, so an abort does
// not wait for r.
func (u *Upload) readFrom(r io.Reader) error {
	for {
		space, gathering, err := u.room()
		if err != nil {
			return err
		}
		n, readErr := r.Read(space)
		if err = u.took(space[:n], gathering); err != nil {
			return err
		}
		switch {
		case errors.Is(readErr, io.EOF):
			return nil
		case readErr != nil:
			return readErr
		}
	}
}

// room is where the next read goes. While the bytes may stay inline it is the
// buffer's free end, and one byte once that end is full; once they go to a file
// it is the whole buffer.
func (u *Upload) room() (space []byte, gathering bool, err error) {
	u.mu.Lock()
	defer u.mu.Unlock()
	if u.ended == nil && u.at == inUploads && u.written >= streamAfter && !u.bufferTried {
		u.growBuffer()
	}
	switch {
	case u.ended != nil:
		return nil, false, u.ended
	case u.at != inMemory || u.call.settings.size > inlineSize:
		return u.buffer[:cap(u.buffer)], false, nil
	case len(u.buffer) < cap(u.buffer):
		return u.buffer[len(u.buffer):cap(u.buffer)], true, nil
	}
	return u.probe[:], false, nil
}

// growBuffer reserves both buffers while replacing one; a tight budget keeps
// streaming with the smaller one.
func (u *Upload) growBuffer() {
	u.bufferTried = true
	reserved, err := u.bucket.store.runtime.ReserveNow(streamBuffer)
	if err != nil {
		return
	}
	u.buffer = make([]byte, 0, streamBuffer)
	u.reserved.Release()
	u.reserved = reserved
}

// took accounts for bytes a read put where room said. In the buffer's free end
// they already lie where they stay; anywhere else they are taken as Write takes
// them.
func (u *Upload) took(p []byte, gathered bool) error {
	u.mu.Lock()
	defer u.mu.Unlock()
	if err := u.alive(); err != nil {
		return err
	}
	var err error
	if gathered {
		err = u.gather(len(p))
	} else {
		err = u.take(p)
	}
	if err != nil {
		u.end(err)
	}
	return err
}

// gather counts n bytes a read put in the buffer's free end
func (u *Upload) gather(n int) error {
	if err := u.bound(n); err != nil {
		return err
	}
	grown := u.buffer[:len(u.buffer)+n]
	u.hash.Write(grown[len(u.buffer):])
	u.buffer = grown
	u.written += int64(n)
	return nil
}

// take checks p against the upload's bounds, then keeps it in the buffer or
// writes it to the file, which it creates when the bytes first pass the buffer
func (u *Upload) take(p []byte) error {
	if err := u.bound(len(p)); err != nil {
		return err
	}
	u.hash.Write(p)
	u.written += int64(len(p))
	if u.at == inMemory && u.fitsInline(len(p)) {
		u.buffer = append(u.buffer, p...)
		return nil
	}
	if u.at == inMemory {
		if err := u.spill(); err != nil {
			return err
		}
	}
	return u.writeFile(p)
}

// bound refuses n more bytes past the declared Size or the bucket's MaxSize
func (u *Upload) bound(n int) error {
	after, declared, most := u.written+int64(n), u.call.settings.size, u.bucket.maxSize
	switch {
	case declared >= 0 && after > declared:
		return fmt.Errorf("%w: a stream longer than its Size of %d", tinystore.ErrInvalid, declared)
	case most > 0 && after > most:
		return &tinystore.LimitError{Name: "bytes of an object, the bucket's MaxSize", Wanted: after, Bound: most}
	}
	return nil
}

// fitsInline says the buffer keeps n more bytes of an object that may stay inline
func (u *Upload) fitsInline(n int) bool {
	return u.call.settings.size <= inlineSize && len(u.buffer)+n <= inlineSize
}

// spill gives the upload its file: an id held from the store's block, its name
// in uploads/, the disk's free space checked and the bytes the buffer gathered.
// After that the buffer only carries bytes to the file.
func (u *Upload) spill() error {
	s := u.bucket.store
	id, err := s.takeID(u.ctx)
	if err != nil {
		return err
	}
	u.id = id
	u.file, err = s.createUpload(id)
	if err != nil {
		return fmt.Errorf("blobs: create %s: %w", uploadName(id), err)
	}
	u.at = inUploads
	if err = s.checkFree(max(u.call.settings.size, u.written)); err != nil {
		return err
	}
	gathered := u.buffer
	u.buffer = u.buffer[:0]
	return u.writeFile(gathered)
}

// writeFile writes to the upload's file, syncing it every 256 MiB, so that no
// commit waits on gigabytes, and checking the disk's free space every 64 MiB
func (u *Upload) writeFile(p []byte) error {
	if _, err := u.file.Write(p); err != nil {
		return fmt.Errorf("blobs: write %s: %w", uploadName(u.id), err)
	}
	if u.written-u.synced >= syncEvery {
		if err := u.file.Sync(); err != nil {
			return fmt.Errorf("blobs: sync %s: %w", uploadName(u.id), err)
		}
		u.synced = u.written
	}
	if u.written-u.checked >= freeEvery {
		if err := u.bucket.store.checkFree(max(u.call.settings.size-u.written, 0)); err != nil {
			return err
		}
		u.checked = u.written
	}
	return nil
}
