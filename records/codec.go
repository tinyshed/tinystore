package records

import (
	"errors"
	"fmt"

	"github.com/klauspost/compress/fse"
	"github.com/klauspost/compress/zstd"
)

// encoder writes columns. It keeps its buffers between columns, so one
// goroutine uses it at a time; the zstd encoder it holds is shared.
type encoder struct {
	zstd      *zstd.Encoder
	fse       fse.Scratch
	symbols   []byte
	rice      []byte
	blob      []byte
	flat      []byte
	numbers   []int64
	lengths   []int64
	inner     []string
	counts    map[int64]int
	words     map[string]int
	stamps    []stamp
	stampEnds []int // where each value's stamps end in stamps
}

func newEncoder(blobs *zstd.Encoder) *encoder {
	return &encoder{zstd: blobs, counts: map[int64]int{}, words: map[string]int{}}
}

// decoder reads columns; one goroutine uses it at a time, and the zstd
// decoder it holds is shared
type decoder struct {
	zstd    *zstd.Decoder
	fse     fse.Scratch
	spelled []byte // a stamp as its layout spells it
}

func newDecoder(blobs *zstd.Decoder) *decoder {
	return &decoder{zstd: blobs}
}

// blobCoders are the zstd coders every encoder and decoder shares.
//
// A segment's text is written once and kept for weeks, so it takes the stronger
// level. A head row waits at most SealAge (an hour unless set), so it takes the
// level that costs an append less.
type blobCoders struct {
	segments, heads *zstd.Encoder
	unpack          *zstd.Decoder
}

func newBlobCoders() (blobCoders, error) {
	segments, err := newBlobEncoder(zstd.SpeedBetterCompression)
	if err != nil {
		return blobCoders{}, err
	}
	heads, err := newBlobEncoder(zstd.SpeedDefault)
	if err != nil {
		return blobCoders{}, errors.Join(err, segments.Close())
	}
	unpack, err := zstd.NewReader(nil, zstd.WithDecoderConcurrency(4),
		zstd.WithDecoderMaxMemory(maxExpandedText), zstd.WithDecoderMaxWindow(maxBlockInput))
	if err != nil {
		return blobCoders{}, errors.Join(fmt.Errorf("records: zstd decoder: %w", err), segments.Close(), heads.Close())
	}
	return blobCoders{segments: segments, heads: heads, unpack: unpack}, nil
}

func (c blobCoders) close() error {
	c.unpack.Close()
	return errors.Join(c.segments.Close(), c.heads.Close())
}

// newBlobEncoder makes an encoder whose EncodeAll may be called concurrently,
// for input no larger than a block's, which always comes out as one frame.
func newBlobEncoder(level zstd.EncoderLevel) (*zstd.Encoder, error) {
	writer, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(2), zstd.WithEncoderLevel(level),
		zstd.WithWindowSize(maxBlockInput), zstd.WithLowerEncoderMem(true), zstd.WithEncoderCRC(false))
	if err != nil {
		return nil, fmt.Errorf("records: zstd encoder: %w", err)
	}
	return writer, nil
}
