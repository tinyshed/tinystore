package records

import (
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

// newBlobCoders makes the zstd pair every encoder and decoder shares: EncodeAll
// and DecodeAll may run concurrently, and nothing larger than a block's input
// is ever one frame
func newBlobCoders() (*zstd.Encoder, *zstd.Decoder, error) {
	writer, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(2), zstd.WithEncoderLevel(zstd.SpeedDefault),
		zstd.WithWindowSize(maxBlockInput), zstd.WithLowerEncoderMem(true), zstd.WithEncoderCRC(false))
	if err != nil {
		return nil, nil, fmt.Errorf("records: zstd encoder: %w", err)
	}
	reader, err := zstd.NewReader(nil, zstd.WithDecoderConcurrency(4),
		zstd.WithDecoderMaxMemory(maxExpandedText), zstd.WithDecoderMaxWindow(maxBlockInput))
	if err != nil {
		_ = writer.Close()
		return nil, nil, fmt.Errorf("records: zstd decoder: %w", err)
	}
	return writer, reader, nil
}
