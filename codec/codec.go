package codec

import (
	"encoding/binary"
	"errors"
	"fmt"
	"hash/crc32"
	"math"
	"sync"

	"github.com/klauspost/compress/zstd"
)

const (
	MaxSamples    = 240
	maxBody       = 8192
	headerSize    = 12
	checksumSize  = 4
	formatVersion = 1
)

var checksumTable = crc32.MakeTable(crc32.Castagnoli)

var ErrInvalid = errors.New("invalid metric payload")

type Sample struct {
	At    int64
	Value float64
}

// Codec owns one bounded encoder and decoder; returned iterators own their buffers.
type Codec struct {
	mu     sync.Mutex
	writer *zstd.Encoder
	reader *zstd.Decoder
	closed bool
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
func (c *Codec) Encode(samples []Sample) ([]byte, error) {
	if len(samples) == 0 || len(samples) > MaxSamples {
		return nil, fmt.Errorf("encode %d samples: expected 1..%d", len(samples), MaxSamples)
	}
	for i := 1; i < len(samples); i++ {
		if samples[i].At <= samples[i-1].At {
			return nil, errors.New("encode samples: timestamps must increase strictly")
		}
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return nil, errors.New("metric codec is closed")
	}
	tsMode, timestamps := encodeTimes(samples)
	valueMode, values := encodeValues(samples)
	best := c.envelope(samples, tsMode, valueMode, timestamps, values)
	// byte compression sometimes prefers raw values to an already packed bitstream
	if (valueMode == valueXOR && len(samples) >= 8) ||
		((valueMode == valueInteger || valueMode == valueScaled) && len(samples) >= 32) {
		raw := rawValues(samples)
		alternative := c.envelope(samples, tsMode, valueRaw, timestamps, raw)
		if len(alternative) < len(best) {
			best = alternative
		}
	}
	return best, nil
}

func (c *Codec) envelope(samples []Sample, tsMode, valueMode byte, timestamps, values []byte) []byte {
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
	out[0], out[1], out[2] = 'T', 'S', formatVersion
	out[3], out[4], out[5] = tsMode, valueMode, flags
	binary.LittleEndian.PutUint16(out[6:], uint16(len(samples)))    //nolint:gosec // encode caps the count at 240
	binary.LittleEndian.PutUint16(out[8:], uint16(len(timestamps))) //nolint:gosec // at most 240 ten-byte varints
	binary.LittleEndian.PutUint16(out[10:], uint16(len(values)))    //nolint:gosec // the raw fallback caps this at 1920 bytes
	out = append(out, body...)
	return binary.LittleEndian.AppendUint32(out, crc32.Checksum(out, checksumTable))
}

// Decode copies the bounded payload; iteration needs neither a lock nor an open Codec.
func (c *Codec) Decode(payload []byte) (*Iterator, error) {
	if len(payload) < headerSize+checksumSize || len(payload) > maxBody+headerSize+checksumSize {
		return nil, fmt.Errorf("%w: payload length", ErrInvalid)
	}
	if payload[0] != 'T' || payload[1] != 'S' || payload[2] != formatVersion ||
		payload[3] > timeDeltaDelta || payload[4] > valueScaled || payload[5] > 1 {
		return nil, fmt.Errorf("%w: format header", ErrInvalid)
	}
	end := len(payload) - checksumSize
	if crc32.Checksum(payload[:end], checksumTable) != binary.LittleEndian.Uint32(payload[end:]) {
		return nil, fmt.Errorf("%w: checksum", ErrInvalid)
	}
	count := int(binary.LittleEndian.Uint16(payload[6:]))
	timeBytes := int(binary.LittleEndian.Uint16(payload[8:]))
	valueBytes := int(binary.LittleEndian.Uint16(payload[10:]))
	leastValueBytes := 8
	if payload[4] == valueScaled {
		leastValueBytes = 9
	}
	if count < 1 || count > MaxSamples || timeBytes < 8 || valueBytes < leastValueBytes ||
		timeBytes+valueBytes > maxBody {
		return nil, fmt.Errorf("%w: decoded bounds", ErrInvalid)
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return nil, errors.New("metric codec is closed")
	}
	var body []byte
	if payload[5] == 1 {
		var err error
		body, err = c.reader.DecodeAll(payload[headerSize:end], nil)
		if err != nil {
			return nil, fmt.Errorf("%w: zstd: %w", ErrInvalid, err)
		}
	} else {
		body = append([]byte(nil), payload[headerSize:end]...)
	}
	if len(body) != timeBytes+valueBytes {
		return nil, fmt.Errorf("%w: stream lengths", ErrInvalid)
	}
	it := &Iterator{
		count: count, timeMode: payload[3], valueMode: payload[4],
		times: body[:timeBytes], values: body[timeBytes:],
	}
	if it.valueMode == valueScaled {
		if it.scale = int(it.values[0]); it.scale > maxScale {
			return nil, fmt.Errorf("%w: scale", ErrInvalid)
		}
		it.values = it.values[1:]
	}
	it.xor = bitReader{data: it.values[8:]}
	return it, nil
}

type Iterator struct {
	count, index, scale int
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
	if it.index == 0 {
		it.at = int64(binary.LittleEndian.Uint64(it.times)) //nolint:gosec // signed timestamp bits are preserved deliberately
		it.times = it.times[8:]
		it.value = binary.LittleEndian.Uint64(it.values)
		it.values = it.values[8:]
		if it.valueMode == valueInteger || it.valueMode == valueScaled {
			it.integer = int64(it.value) //nolint:gosec // signed integer bits are preserved deliberately
			if it.err = it.fromInteger(); it.err != nil {
				return false
			}
		}
		if it.timeMode == timeFixed && it.count > 1 {
			it.delta, it.err = takeUnsigned(&it.times)
		}
	} else {
		it.err = it.nextTime()
		if it.err == nil {
			it.err = it.nextValue()
		}
	}
	if it.err != nil {
		return false
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
	if len(it.times) != 0 {
		return fmt.Errorf("%w: trailing timestamps", ErrInvalid)
	}
	if it.valueMode == valueXOR {
		if !it.xor.finished() {
			return fmt.Errorf("%w: trailing XOR bits", ErrInvalid)
		}
	} else if len(it.values) != 0 ||
		((it.valueMode == valueInteger || it.valueMode == valueScaled) && it.word != 0) {
		return fmt.Errorf("%w: trailing values", ErrInvalid)
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
