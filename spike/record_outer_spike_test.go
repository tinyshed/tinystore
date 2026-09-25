package spike

import (
	"errors"

	"github.com/klauspost/compress/zstd"
)

func (c *recordBlockCodec) compressRecordSegment(segment []byte) ([]byte, error) {
	interior := segment[3 : len(segment)-4]
	if len(interior) > recordWorkLimit {
		return segment, nil
	}
	if c.outerWriter == nil {
		writer, err := zstd.NewWriter(nil, zstd.WithEncoderLevel(zstd.SpeedBestCompression),
			zstd.WithWindowSize(recordByteLimit), zstd.WithEncoderConcurrency(1), zstd.WithLowerEncoderMem(true))
		if err != nil {
			return nil, err
		}
		c.outerWriter = writer
	}
	return recordSegmentEnvelope(4, c.outerWriter.EncodeAll(interior, nil)), nil
}

func (c *recordBlockCodec) readCompressedRecordSegment(payload []byte) ([]recordEvent, error) {
	interior, err := c.reader.DecodeAll(payload, nil)
	if err != nil {
		return nil, err
	}
	if len(interior) == 0 || len(interior) > recordWorkLimit || interior[0] > 3 {
		return nil, errors.New("record segment expansion or recursive envelope")
	}
	return c.decodeRecordSegment(recordSegmentEnvelope(interior[0], interior[1:]))
}
