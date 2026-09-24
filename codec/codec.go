package codec

import (
	"encoding/binary"
	"errors"
	"fmt"
	"hash/crc32"
	"math"
	"sync"

	"github.com/klauspost/compress/huff0"
	"github.com/klauspost/compress/zstd"
)

const (
	MaxSamples    = 240
	maxBody       = 8192
	headerSize    = 4
	checksumSize  = 4
	formatVersion = 1
)

var checksumTable = crc32.MakeTable(crc32.Castagnoli)

var ErrInvalid = errors.New("invalid metric payload")

type Sample struct {
	At    int64
	Value float64
}

// Head is what the row around a body already holds. Encode returns it, the
// caller stores it, and Decode is given it back; nothing in the body repeats it.
type Head struct {
	Start, End int64
	Count      int
	First      float64
}

func (h Head) bytes() []byte {
	out := make([]byte, 0, 26)
	out = binary.LittleEndian.AppendUint64(out, uint64(h.Start)) //nolint:gosec // the signed bits, kept
	out = binary.LittleEndian.AppendUint64(out, uint64(h.End))   //nolint:gosec // the signed bits, kept
	out = binary.LittleEndian.AppendUint16(out, uint16(h.Count)) //nolint:gosec // at most MaxSamples
	return binary.LittleEndian.AppendUint64(out, math.Float64bits(h.First))
}

// step is what a fixed-step block does not write down
func (h Head) step() (uint64, bool) {
	if h.Count < 2 {
		return 0, false
	}
	span := distance(h.Start, h.End)
	divisor := uint64(h.Count - 1) //nolint:gosec // Count is at least two here
	if span == 0 || span%divisor != 0 {
		return 0, false
	}
	return span / divisor, true
}

// Codec owns one bounded encoder and decoder; returned iterators own their buffers.
type Codec struct {
	mu       sync.Mutex
	writer   *zstd.Encoder
	reader   *zstd.Decoder
	huffman  huff0.Scratch
	unpacked huff0.Scratch
	closed   bool
}

func New() (*Codec, error) {
	writer, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1),
		zstd.WithWindowSize(maxBody), zstd.WithLowerEncoderMem(true))
	if err != nil {
		return nil, fmt.Errorf("create metric encoder: %w", err)
	}
	reader, err := zstd.NewReader(nil, zstd.WithDecoderConcurrency(1),
		zstd.WithDecoderMaxMemory(maxBody), zstd.WithDecoderMaxWindow(maxBody))
	if err != nil {
		_ = writer.Close()
		return nil, fmt.Errorf("create metric decoder: %w", err)
	}
	return &Codec{writer: writer, reader: reader}, nil
}

func (c *Codec) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return nil
	}
	c.closed = true
	c.reader.Close()
	return c.writer.Close()
}

// Encode requires strictly increasing timestamps and preserves every float64 bit.
func (c *Codec) Encode(samples []Sample) (Head, []byte, error) {
	if len(samples) == 0 || len(samples) > MaxSamples {
		return Head{}, nil, fmt.Errorf("encode %d samples: expected 1..%d", len(samples), MaxSamples)
	}
	for i := 1; i < len(samples); i++ {
		if samples[i].At <= samples[i-1].At {
			return Head{}, nil, errors.New("encode samples: timestamps must increase strictly")
		}
	}
	head := Head{
		Start: samples[0].At,
		End:   samples[len(samples)-1].At,
		Count: len(samples),
		First: samples[0].Value,
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return Head{}, nil, errors.New("metric codec is closed")
	}
	tsMode, timestamps := encodeTimes(head, samples)
	valueMode, values := c.encodeValues(samples)
	best := c.envelope(head, tsMode, valueMode, timestamps, values)
	// byte compression sometimes prefers raw values to an already packed bitstream
	if (valueMode == valueXOR && len(samples) >= 8) ||
		((valueMode == valueInteger || valueMode == valueScaled) && len(samples) >= 32) {
		raw := rawValues(samples)
		alternative := c.envelope(head, tsMode, valueRaw, timestamps, raw)
		if len(alternative) < len(best) {
			best = alternative
		}
	}
	return head, best, nil
}

func (c *Codec) envelope(head Head, tsMode, valueMode byte, timestamps, values []byte) []byte {
	body := append(append(make([]byte, 0, len(timestamps)+len(values)), timestamps...), values...)
	flags := byte(0)
	if len(body) >= 48 {
		compressed := c.writer.EncodeAll(body, nil)
		if len(compressed) < len(body) {
			body = compressed
			flags = 1
		}
	}
	out := make([]byte, headerSize, headerSize+len(body)+checksumSize)
	out[0] = formatVersion
	out[1] = tsMode | valueMode<<2 | flags<<5
	binary.LittleEndian.PutUint16(out[2:], uint16(len(timestamps))) //nolint:gosec // at most 240 ten-byte varints
	out = append(out, body...)
	sum := crc32.Update(crc32.Checksum(head.bytes(), checksumTable), checksumTable, out)
	return binary.LittleEndian.AppendUint32(out, sum)
}

// Decode copies the bounded body; iteration needs neither a lock nor an open Codec.
func (c *Codec) Decode(head Head, body []byte) (*Iterator, error) {
	if head.Count < 1 || head.Count > MaxSamples || head.End < head.Start ||
		(head.Count == 1 && head.End != head.Start) {
		return nil, fmt.Errorf("%w: head", ErrInvalid)
	}
	if len(body) < headerSize+checksumSize || len(body) > maxBody+headerSize+checksumSize {
		return nil, fmt.Errorf("%w: body length", ErrInvalid)
	}
	if body[0] != formatVersion {
		return nil, fmt.Errorf("%w: format version", ErrInvalid)
	}
	if err := checkFlags(body[1]); err != nil {
		return nil, err
	}
	end := len(body) - checksumSize
	sum := crc32.Update(crc32.Checksum(head.bytes(), checksumTable), checksumTable, body[:end])
	if sum != binary.LittleEndian.Uint32(body[end:]) {
		return nil, fmt.Errorf("%w: checksum", ErrInvalid)
	}
	timeBytes := int(binary.LittleEndian.Uint16(body[2:]))
	return c.decodeStream(head, body[1], timeBytes, body[headerSize:end])
}

// EncodeValues packs values alone, for a caller that keeps the timestamps and
// the first value itself. It returns Encode's stream for evenly spaced samples,
// which write no timestamps, behind the byte that says how it was packed:
//
//	Encode        1 | flags | 0 0 | stream | checksum
//	EncodeValues      flags |       stream
func (c *Codec) EncodeValues(values []float64) ([]byte, error) {
	samples := make([]Sample, len(values))
	for i, value := range values {
		samples[i] = Sample{At: int64(i), Value: value}
	}
	_, body, err := c.Encode(samples)
	if err != nil {
		return nil, err
	}
	return append([]byte{body[1]}, body[headerSize:len(body)-checksumSize]...), nil
}

// DecodeValues reads what EncodeValues wrote, given the first value and the
// count, which the stream does not repeat. It carries no checksum of its own, so
// the caller checks the bytes it stored; each Sample's At is its index.
func (c *Codec) DecodeValues(first float64, count int, stream []byte) (*Iterator, error) {
	if count < 1 || count > MaxSamples {
		return nil, fmt.Errorf("%w: value count", ErrInvalid)
	}
	if len(stream) < 1 || len(stream) > maxBody+1 {
		return nil, fmt.Errorf("%w: value stream length", ErrInvalid)
	}
	if err := checkFlags(stream[0]); err != nil {
		return nil, err
	}
	if stream[0]&3 != timeFixed {
		return nil, fmt.Errorf("%w: value stream with timestamps", ErrInvalid)
	}
	head := Head{Start: 0, End: int64(count - 1), Count: count, First: first}
	return c.decodeStream(head, stream[0], 0, stream[1:])
}

func checkFlags(flags byte) error {
	timeMode, valueMode, compressed := flags&3, flags>>2&7, flags>>5
	if timeMode > timeDeltaDelta || valueMode > valueScaled || compressed > 1 {
		return fmt.Errorf("%w: format header", ErrInvalid)
	}
	return nil
}

// decodeStream inflates a stream when it is compressed and prepares an
// iterator over its timestamps, the first timeBytes of it, and its values.
func (c *Codec) decodeStream(head Head, flags byte, timeBytes int, payload []byte) (*Iterator, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return nil, errors.New("metric codec is closed")
	}
	var stream []byte
	if flags>>5 == 1 {
		var err error
		stream, err = c.reader.DecodeAll(payload, nil)
		if err != nil {
			return nil, fmt.Errorf("%w: zstd: %w", ErrInvalid, err)
		}
	} else {
		stream = append([]byte(nil), payload...)
	}
	if timeBytes > len(stream) || len(stream) > maxBody {
		return nil, fmt.Errorf("%w: stream lengths", ErrInvalid)
	}

	it := &Iterator{
		count: head.Count, head: head, timeMode: flags & 3, valueMode: flags >> 2 & 7,
		times: stream[:timeBytes], values: stream[timeBytes:],
		at: head.Start, value: math.Float64bits(head.First),
	}
	if err := c.prepare(it); err != nil {
		return nil, err
	}
	return it, nil
}

// prepare reads whatever each representation keeps ahead of its samples
func (c *Codec) prepare(it *Iterator) error {
	if it.timeMode == timeFixed && it.count > 1 {
		step, ok := it.head.step()
		if !ok {
			return fmt.Errorf("%w: fixed step", ErrInvalid)
		}
		it.delta = step
	}
	switch it.valueMode {
	case valueInteger, valueScaled:
		if len(it.values) < 1 {
			return fmt.Errorf("%w: value stream", ErrInvalid)
		}
		if it.deltas = it.values[0]; it.deltas > deltasInHuffman {
			return fmt.Errorf("%w: delta packing", ErrInvalid)
		}
		it.values = it.values[1:]
		if it.valueMode == valueScaled {
			if len(it.values) < 1 {
				return fmt.Errorf("%w: value stream", ErrInvalid)
			}
			if it.scale = int(it.values[0]); it.scale > maxScale {
				return fmt.Errorf("%w: scale", ErrInvalid)
			}
			it.values = it.values[1:]
		}
		if it.deltas == deltasInHuffman && it.count > 1 {
			expanded, err := c.readHuffman(it.values)
			if err != nil {
				return err
			}
			it.values = expanded
		}
		return it.seedInteger()
	case valueXOR:
		it.xor = bitReader{data: it.values}
	}
	return nil
}

// readHuffman expands the deltas, table and all
func (c *Codec) readHuffman(values []byte) ([]byte, error) {
	table, rest, err := huff0.ReadTable(values, &c.unpacked)
	if err != nil {
		return nil, fmt.Errorf("%w: huffman table: %w", ErrInvalid, err)
	}
	table.MaxDecodedSize = maxBody
	expanded, err := table.Decompress1X(rest)
	if err != nil {
		return nil, fmt.Errorf("%w: huffman stream: %w", ErrInvalid, err)
	}
	return append(make([]byte, 0, len(expanded)), expanded...), nil
}

type Iterator struct {
	count, index, scale int
	deltas              byte
	head                Head
	timeMode, valueMode byte
	times, values       []byte
	at                  int64
	delta               uint64
	value               uint64
	integer             int64
	word                uint64
	wordLeft            int
	wordBits            uint8
	xor                 bitReader
	leading, trailing   uint8
	window              bool
	sample              Sample
	err                 error
}

func (it *Iterator) Next() bool {
	if it.err != nil || it.index >= it.count {
		return false
	}
	if it.index > 0 {
		it.err = it.nextTime()
		if it.err == nil {
			it.err = it.nextValue()
		}
		if it.err != nil {
			return false
		}
	}
	it.index++
	if it.index == it.count {
		if it.err = it.finish(); it.err != nil {
			return false
		}
	}
	it.sample = Sample{At: it.at, Value: math.Float64frombits(it.value)}
	return true
}

func (it *Iterator) Sample() Sample { return it.sample }
func (it *Iterator) Err() error     { return it.err }

func (it *Iterator) finish() error {
	if it.at != it.head.End {
		return fmt.Errorf("%w: last timestamp is not the head's", ErrInvalid)
	}
	if len(it.times) != 0 {
		return fmt.Errorf("%w: trailing timestamps", ErrInvalid)
	}
	switch {
	case it.valueMode == valueXOR:
		if !it.xor.finished() {
			return fmt.Errorf("%w: trailing XOR bits", ErrInvalid)
		}
	case len(it.values) != 0:
		return fmt.Errorf("%w: trailing values", ErrInvalid)
	case it.deltas == deltasInWords && it.word != 0 &&
		(it.valueMode == valueInteger || it.valueMode == valueScaled):
		return fmt.Errorf("%w: integer word padding", ErrInvalid)
	}
	return nil
}

func takeUnsigned(data *[]byte) (uint64, error) {
	v, n := binary.Uvarint(*data)
	if n <= 0 {
		return 0, fmt.Errorf("%w: unsigned varint", ErrInvalid)
	}
	*data = (*data)[n:]
	return v, nil
}

func takeSigned(data *[]byte) (int64, error) {
	v, n := binary.Varint(*data)
	if n <= 0 {
		return 0, fmt.Errorf("%w: signed varint", ErrInvalid)
	}
	*data = (*data)[n:]
	return v, nil
}
