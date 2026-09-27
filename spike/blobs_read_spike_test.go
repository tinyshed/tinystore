package spike

import (
	"bytes"
	"compress/flate"
	"crypto/sha256"
	"errors"
	"fmt"
	"hash/crc32"
	"io"
	"math/rand/v2"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// TestBlobsHashes times what a check or a backup spends on a byte, on 256 MiB
// of random bytes: SHA-256, CRC-32C and deflate at its fastest and default
func TestBlobsHashes(t *testing.T) {
	kvMeasuring(t)
	data := blobsBytes(256, 256<<20)
	for _, way := range []struct {
		name string
		run  func() error
	}{
		{"SHA-256", func() error { sha256.Sum256(data); return nil }},
		{"CRC-32C", func() error { crc32.Checksum(data, crc32.MakeTable(crc32.Castagnoli)); return nil }},
		{"deflate, BestSpeed", func() error { return blobsDeflate(data, flate.BestSpeed) }},
		{"deflate, default", func() error { return blobsDeflate(data, flate.DefaultCompression) }},
	} {
		began := time.Now()
		if err := way.run(); err != nil {
			t.Fatal(err)
		}
		t.Logf("%-20s %5.2f GB/s of one core", way.name, float64(len(data))/time.Since(began).Seconds()/1e9)
	}
}

func blobsDeflate(data []byte, level int) error {
	writer, err := flate.NewWriter(io.Discard, level)
	if err != nil {
		return err
	}
	_, err = writer.Write(data)
	return errorsJoin(err, writer.Close())
}

// the block a checked range reads whole and verifies
const blobsBlock = 64 << 10

// blobsChecked is a reader of a file whose every 64 KiB block has a CRC-32C:
// a read takes the blocks its range touches, whole, and checks each
type blobsChecked struct {
	file   *os.File
	size   int64
	sums   []uint32
	block  []byte
	offset int64
}

var blobsCastagnoli = crc32.MakeTable(crc32.Castagnoli)

func (c *blobsChecked) ReadAt(p []byte, off int64) (int, error) {
	read := 0
	for read < len(p) && off+int64(read) < c.size {
		at := off + int64(read)
		first := at / blobsBlock * blobsBlock
		n, err := c.file.ReadAt(c.block[:min(blobsBlock, c.size-first)], first)
		if err != nil && !errors.Is(err, io.EOF) {
			return read, err
		}
		if crc32.Checksum(c.block[:n], blobsCastagnoli) != c.sums[first/blobsBlock] {
			return read, fmt.Errorf("block at %d changed", first)
		}
		read += copy(p[read:], c.block[at-first:n])
	}
	if read < len(p) {
		return read, io.EOF
	}
	return read, nil
}

func (c *blobsChecked) Read(p []byte) (int, error) {
	n, err := c.ReadAt(p, c.offset)
	c.offset += int64(n)
	return n, err
}

func (c *blobsChecked) Seek(offset int64, whence int) (int64, error) {
	switch whence {
	case io.SeekStart:
		c.offset = offset
	case io.SeekCurrent:
		c.offset += offset
	case io.SeekEnd:
		c.offset = c.size + offset
	}
	return c.offset, nil
}

// blobsWhole is a reader of a file that hashes what it reads in order from its
// first byte, and fails the read that reaches the end when the hash is not the
// content's; a read anywhere else is a range, which is not checked
type blobsWhole struct {
	file *os.File
	size int64
	want [32]byte
	hash interface {
		io.Writer
		Sum([]byte) []byte
	}
	at, hashed int64
}

func (w *blobsWhole) Read(p []byte) (int, error) {
	n, err := w.file.Read(p)
	if w.at == 0 && w.hashed > 0 {
		w.hash, w.hashed = sha256.New(), 0 // read again from the start, as a sniff of its type does
	}
	if w.at == w.hashed {
		_, _ = w.hash.Write(p[:n])
		w.hashed += int64(n)
		if w.hashed == w.size && !bytes.Equal(w.hash.Sum(nil), w.want[:]) {
			return 0, errors.New("the content changed")
		}
	}
	w.at += int64(n)
	return n, err
}

func (w *blobsWhole) Seek(offset int64, whence int) (int64, error) {
	at, err := w.file.Seek(offset, whence)
	w.at = at
	return at, err
}

// TestBlobsServe serves a 1 GiB object through http.ServeContent whole and in
// ranges of 4 MiB at random offsets, from the file as it is, from a reader
// that checks a whole read against its SHA-256, and from one that checks each
// 64 KiB block against its CRC-32C
func TestBlobsServe(t *testing.T) {
	kvMeasuring(t)
	const size = 1 << 30
	path := filepath.Join(blobsDir(t), "object")
	data := blobsBytes(1, size)
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
	want := sha256.Sum256(data)
	sums := make([]uint32, 0, size/blobsBlock)
	for first := 0; first < size; first += blobsBlock {
		sums = append(sums, crc32.Checksum(data[first:first+blobsBlock], blobsCastagnoli))
	}
	for _, way := range []string{"as it is", "whole SHA-256", "CRC-32C a block"} {
		open := func() (io.ReadSeeker, func() error) {
			file, err := os.Open(path)
			if err != nil {
				t.Fatal(err)
			}
			switch way {
			case "whole SHA-256":
				return &blobsWhole{file: file, size: size, want: want, hash: sha256.New()}, file.Close
			case "CRC-32C a block":
				return &blobsChecked{file: file, size: size, sums: sums, block: make([]byte, blobsBlock)}, file.Close
			}
			return file, file.Close
		}
		blobsServeWhole(t, way, size, open)
		blobsServeRanges(t, way, size, open)
	}
}

func blobsServeWhole(t *testing.T, way string, size int64, open func() (io.ReadSeeker, func() error)) {
	t.Helper()
	began := time.Now()
	for range 3 {
		content, done := open()
		recorder := blobsDiscard{header: http.Header{"Content-Type": {"video/mp4"}}}
		request := httptest.NewRequest(http.MethodGet, "/object", nil)
		http.ServeContent(&recorder, request, "", time.Time{}, content)
		if err := done(); err != nil || recorder.written != size {
			t.Fatalf("%s: served %d of %d bytes: %v", way, recorder.written, size, err)
		}
	}
	t.Logf("%-16s whole    %5.2f GB/s", way, float64(3*size)/time.Since(began).Seconds()/1e9)
}

func blobsServeRanges(t *testing.T, way string, size int64, open func() (io.ReadSeeker, func() error)) {
	t.Helper()
	const ranges, length = 1000, 4 << 20
	random := rand.New(rand.NewPCG(1, 2))
	began := time.Now()
	for range ranges {
		content, done := open()
		from := random.Int64N(size - length)
		recorder := blobsDiscard{header: http.Header{"Content-Type": {"video/mp4"}}}
		request := httptest.NewRequest(http.MethodGet, "/object", nil)
		request.Header.Set("Range", fmt.Sprintf("bytes=%d-%d", from, from+length-1))
		http.ServeContent(&recorder, request, "", time.Time{}, content)
		if err := done(); err != nil || recorder.written != length || recorder.status != http.StatusPartialContent {
			t.Fatalf("%s: a range answered %d with %d bytes: %v", way, recorder.status, recorder.written, err)
		}
	}
	elapsed := time.Since(began)
	t.Logf("%-16s ranges   %6.0f of 4 MiB a second, %5.2f GB/s", way, ranges/elapsed.Seconds(),
		float64(ranges*length)/elapsed.Seconds()/1e9)
}

// blobsDiscard is a response that counts its body and keeps nothing
type blobsDiscard struct {
	header  http.Header
	status  int
	written int64
}

func (d *blobsDiscard) Header() http.Header { return d.header }

func (d *blobsDiscard) WriteHeader(status int) { d.status = status }

func (d *blobsDiscard) Write(p []byte) (int, error) {
	d.written += int64(len(p))
	return len(p), nil
}
