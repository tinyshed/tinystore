package blobs

import (
	"bytes"
	"errors"
	"fmt"
	"hash"
	"io"
	"os"

	"github.com/tinyshed/tinystore"
)

// Reader is an object as Open found it: its fields, and its bytes through
// Read, ReadAt and Seek, so that http.ServeContent serves it with its ranges
// and conditions. A reader of a file holds the file open and nothing of the
// engine's; an inline object's reader holds its bytes. ReadAt may be called
// from several goroutines, Read and Seek from one.
type Reader struct {
	Object
	bucket, path string
	file         *os.File // nil for an inline object
	body         []byte   // an inline object's bytes, checked at Open
	want         []byte   // the content's SHA-256, which a file's whole read is checked against
	offset       int64    // where the next Read begins
	hash         hash.Hash
	hashed       int64                  // how far the bytes read in order from the first have been hashed
	reserved     *tinystore.Reservation // an inline object's bytes, in the store's memory
	closed       bool
}

// Read reads on from where the last Read or Seek left off. A file read from
// its first byte to its last in order is checked: the Read that would hand
// over the last bytes of a file that does not hash to the object's SHA-256 is
// tinystore.ErrCorrupt instead, so a copy of a changed object fails before
// its end.
func (r *Reader) Read(p []byte) (int, error) {
	if r.offset >= r.Size {
		return 0, io.EOF
	}
	n, err := r.ReadAt(p, r.offset)
	if checkErr := r.check(p[:n]); checkErr != nil {
		return 0, checkErr
	}
	r.offset += int64(n)
	return n, err
}

// check hashes the bytes of a read that carries on from the bytes hashed
// before it, and compares the hash once they reach the last byte
//
//	Read [0, 512), Seek 0, Read [0, 4096) → hashed to 4096, the first 512 once
//	Seek 1000, Read                        → a range: not hashed
func (r *Reader) check(p []byte) error {
	end := r.offset + int64(len(p))
	if r.file == nil || r.offset > r.hashed || end <= r.hashed {
		return nil
	}
	r.hash.Write(p[r.hashed-r.offset:])
	r.hashed = end
	if r.hashed == r.Size && !bytes.Equal(r.hash.Sum(nil), r.want) {
		return r.corrupt("its bytes do not hash to its SHA-256")
	}
	return nil
}

// ReadAt reads len(p) bytes at off, and io.EOF where the object ends; like
// any range it is not checked. A file that ends before its object does is
// tinystore.ErrCorrupt.
func (r *Reader) ReadAt(p []byte, off int64) (int, error) {
	if off < 0 {
		return 0, fmt.Errorf("%w: blobs: a read at %d", tinystore.ErrInvalid, off)
	}
	if off >= r.Size {
		return 0, io.EOF
	}
	want, last := p, false
	if left := r.Size - off; int64(len(p)) >= left {
		want, last = p[:left], true
	}
	var n int
	var err error
	if r.file == nil {
		n = copy(want, r.body[off:])
	} else {
		n, err = r.file.ReadAt(want, off)
	}
	switch {
	case n < len(want) && (err == nil || errors.Is(err, io.EOF)):
		return n, r.corrupt("its file ends before it does")
	case n < len(want):
		return n, err
	case last:
		return n, io.EOF
	}
	return n, nil
}

// Seek sets where the next Read begins; a position past the end reads io.EOF.
func (r *Reader) Seek(offset int64, whence int) (int64, error) {
	switch whence {
	case io.SeekStart:
	case io.SeekCurrent:
		offset += r.offset
	case io.SeekEnd:
		offset += r.Size
	default:
		return 0, fmt.Errorf("%w: blobs: a seek from %d", tinystore.ErrInvalid, whence)
	}
	if offset < 0 {
		return 0, fmt.Errorf("%w: blobs: a seek to %d", tinystore.ErrInvalid, offset)
	}
	r.offset = offset
	return offset, nil
}

// Close closes the reader's file and gives back the memory an inline object's
// bytes held; a second Close does nothing.
func (r *Reader) Close() error {
	if r.closed {
		return nil
	}
	r.closed = true
	if r.reserved != nil {
		r.reserved.Release()
	}
	if r.file == nil {
		return nil
	}
	return r.file.Close()
}

func (r *Reader) corrupt(why string) error {
	return &KeyError{Bucket: r.bucket, Path: r.path, Err: fmt.Errorf("%w: %s", tinystore.ErrCorrupt, why)}
}
