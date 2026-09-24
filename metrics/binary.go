package metrics

import (
	"encoding/binary"
	"fmt"
	"math"
	"sync"

	"github.com/klauspost/compress/zstd"
)

// binaryReader turns malformed input into one error before callers allocate from its lengths
type binaryReader struct {
	data []byte
	err  error
}

func (r *binaryReader) take(size int) []byte {
	if r.err != nil {
		return nil
	}
	if size < 0 || size > len(r.data) {
		r.err = fmt.Errorf("%w: truncated binary field", ErrCorrupt)
		return nil
	}
	value := r.data[:size]
	r.data = r.data[size:]
	return value
}

func (r *binaryReader) byte() byte {
	data := r.take(1)
	if len(data) == 0 {
		return 0
	}
	return data[0]
}

func (r *binaryReader) unsigned() uint64 {
	if r.err != nil {
		return 0
	}
	value, n := binary.Uvarint(r.data)
	if n <= 0 {
		r.err = fmt.Errorf("%w: invalid varint", ErrCorrupt)
		return 0
	}
	r.data = r.data[n:]
	return value
}

func (r *binaryReader) size(maximum int) int {
	value := r.unsigned()
	if maximum < 0 || value > uint64(maximum) {
		r.err = fmt.Errorf("%w: binary size exceeds limit", ErrCorrupt)
		return 0
	}
	return int(value) //nolint:gosec // checked against a nonnegative int bound
}

func (r *binaryReader) word() uint64 {
	data := r.take(8)
	if len(data) != 8 {
		return 0
	}
	return binary.LittleEndian.Uint64(data)
}

func (r *binaryReader) finish() error {
	if r.err != nil {
		return r.err
	}
	if len(r.data) != 0 {
		return fmt.Errorf("%w: trailing binary fields", ErrCorrupt)
	}
	return nil
}

// foldSigned is zigzag: small magnitudes of either sign become small numbers.
func foldSigned(value int64) uint64 {
	return uint64(value)<<1 ^ uint64(value>>63) //nolint:gosec // a bit transform, not a value converted
}

func unfoldSigned(value uint64) int64 {
	return int64(value>>1) ^ -int64(value&1)
}

func appendFloat(out []byte, value float64) []byte {
	for tag, factor := range []float64{1, 100} {
		product := math.Round(value * factor)
		if math.IsNaN(product) || math.IsInf(product, 0) || math.Abs(product) >= 0x1p60 {
			continue
		}
		integer := int64(product)
		if math.Float64bits(float64(integer)/factor) == math.Float64bits(value) {
			return binary.AppendUvarint(out, foldSigned(integer)<<2|uint64(tag))
		}
	}
	return binary.LittleEndian.AppendUint64(append(out, 2), math.Float64bits(value))
}

func (r *binaryReader) float() float64 {
	token := r.unsigned()
	if token == 2 {
		return math.Float64frombits(r.word())
	}
	tag := token & 3
	if tag > 1 {
		r.err = fmt.Errorf("%w: scalar tag", ErrCorrupt)
		return 0
	}
	value := float64(unfoldSigned(token >> 2))
	if tag == 1 {
		value /= 100
	}
	return value
}

type metadataCodec struct {
	writeMu, readMu sync.Mutex
	writer          *zstd.Encoder
	reader          *zstd.Decoder
}

func newMetadataCodec() (*metadataCodec, error) {
	w, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1),
		zstd.WithWindowSize(8192), zstd.WithLowerEncoderMem(true))
	if err != nil {
		return nil, fmt.Errorf("create metadata encoder: %w", err)
	}
	r, err := zstd.NewReader(nil, zstd.WithDecoderConcurrency(1),
		zstd.WithDecoderMaxMemory(8192), zstd.WithDecoderMaxWindow(8192))
	if err != nil {
		_ = w.Close()
		return nil, fmt.Errorf("create metadata decoder: %w", err)
	}
	return &metadataCodec{writer: w, reader: r}, nil
}

func (m *metadataCodec) encode(data []byte) []byte {
	m.writeMu.Lock()
	defer m.writeMu.Unlock()
	packed := m.writer.EncodeAll(data, nil)
	if len(packed) < len(data) {
		return append([]byte{1}, packed...)
	}
	return append([]byte{0}, data...)
}

func (m *metadataCodec) decode(data []byte) ([]byte, error) {
	if len(data) < 1 || len(data) > maxDirectoryBytes {
		return nil, fmt.Errorf("%w: compressed field size", ErrCorrupt)
	}
	if data[0] == 0 {
		return data[1:], nil
	}
	if data[0] != 1 {
		return nil, fmt.Errorf("%w: compression mode", ErrCorrupt)
	}
	m.readMu.Lock()
	defer m.readMu.Unlock()
	plain, err := m.reader.DecodeAll(data[1:], nil)
	if err != nil {
		return nil, fmt.Errorf("%w: compressed metadata: %w", ErrCorrupt, err)
	}
	if len(plain) > maxDirectoryBytes {
		return nil, fmt.Errorf("%w: expanded metadata", ErrCorrupt)
	}
	return plain, nil
}

func (m *metadataCodec) close() error {
	m.reader.Close()
	if err := m.writer.Close(); err != nil {
		return fmt.Errorf("close metadata encoder: %w", err)
	}
	return nil
}
