package codec

import (
	"encoding/binary"
	"fmt"
	"math"
	"math/bits"

	"github.com/klauspost/compress/huff0"
)

const (
	valueRaw byte = iota
	valueConst
	valueInteger
	valueXOR
	valueScaled
)

// how far the scale search goes: past nine, our fixtures gained no bytes and
// cost five times the encode, which is a measurement and not a law
const maxScale = 9

var pow10 = [maxScale + 1]float64{1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9}

// rawValues leaves the first value out, because the head already carries it
func rawValues(samples []Sample) []byte {
	out := make([]byte, 0, 8*(len(samples)-1))
	for _, s := range samples[1:] {
		out = binary.LittleEndian.AppendUint64(out, math.Float64bits(s.Value))
	}
	return out
}

func (c *Codec) encodeValues(samples []Sample) (byte, []byte) {
	first := math.Float64bits(samples[0].Value)
	constant := true
	for _, s := range samples[1:] {
		constant = constant && math.Float64bits(s.Value) == first
	}
	if constant {
		return valueConst, nil
	}
	mode, best := valueRaw, rawValues(samples)
	integer := c.encodeIntegers(samples)
	if integer != nil && len(integer) < len(best) {
		mode, best = valueInteger, integer
	}
	if integer == nil {
		if scaled := c.encodeScaled(samples); scaled != nil && len(scaled) < len(best) {
			mode, best = valueScaled, scaled
		}
	}
	if xor := encodeXOR(samples); len(xor) < len(best) {
		mode, best = valueXOR, xor
	}
	return mode, best
}

// encodeScaled turns decimals into the integers they were written as, and
// refuses the block unless the decoder's own expression returns the original
// bits for every sample.
func (c *Codec) encodeScaled(samples []Sample) []byte {
	scale := 0
	for _, s := range samples {
		needed := exactScale(s.Value)
		if needed < 0 {
			return nil
		}
		scale = max(scale, needed)
	}
	if scale == 0 {
		return nil
	}
	factor := pow10[scale]
	deltas := make([]uint64, 0, len(samples)-1)
	var previous int64
	for i, s := range samples {
		scaled := math.Round(s.Value * factor) // truncating loses 0.29, whose product is 28.999999999999996
		if scaled < -0x1p63 || scaled >= 0x1p63 {
			return nil
		}
		current := int64(scaled)
		if math.Float64bits(float64(current)/factor) != math.Float64bits(s.Value) {
			return nil
		}
		if i > 0 {
			delta := current - previous
			if (current > previous && delta < 0) || (current < previous && delta > 0) {
				return nil
			}
			zigzag := foldSigned(delta)
			if zigzag >= 1<<60 {
				return nil
			}
			deltas = append(deltas, zigzag)
		}
		previous = current
	}
	return c.packDeltas([]byte{byte(scale)}, deltas)
}

// exactScale is the smallest power of ten that survives the round trip, or -1
// for a value no power of ten reproduces, which includes negative zero.
func exactScale(v float64) int {
	if math.IsNaN(v) || math.IsInf(v, 0) {
		return -1
	}
	for scale := range maxScale + 1 {
		scaled := math.Round(v * pow10[scale])
		if scaled < -0x1p63 || scaled >= 0x1p63 {
			return -1
		}
		if math.Float64bits(float64(int64(scaled))/pow10[scale]) == math.Float64bits(v) {
			return scale
		}
	}
	return -1
}

func (c *Codec) encodeIntegers(samples []Sample) []byte {
	deltas := make([]uint64, 0, len(samples)-1)
	var previous int64
	for i, s := range samples {
		if math.IsNaN(s.Value) || s.Value < -0x1p63 || s.Value >= 0x1p63 {
			return nil
		}
		current := int64(s.Value)
		if math.Float64bits(float64(current)) != math.Float64bits(s.Value) {
			return nil
		}
		if i > 0 {
			delta := current - previous
			if (current > previous && delta < 0) || (current < previous && delta > 0) {
				return nil
			}
			zigzag := foldSigned(delta)
			if zigzag >= 1<<60 {
				return nil
			}
			deltas = append(deltas, zigzag)
		}
		previous = current
	}
	return c.packDeltas(nil, deltas)
}

var (
	wordCounts = [...]int{240, 120, 60, 30, 20, 15, 12, 10, 8, 7, 6, 5, 4, 3, 2, 1}
	wordWidths = [...]uint8{0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 15, 20, 30, 60}
)

const (
	deltasInWords byte = iota
	deltasInHuffman
)

// packDeltas writes the deltas both ways and keeps the smaller, because
// Simple8b pays for the widest value in a word and Huffman for the rarest
func (c *Codec) packDeltas(prefix []byte, deltas []uint64) []byte {
	words := packIntegers(append([]byte{deltasInWords}, prefix...), deltas)
	huffman := c.packHuffman(append([]byte{deltasInHuffman}, prefix...), deltas)
	if huffman != nil && len(huffman) < len(words) {
		return huffman
	}
	return words
}

func (c *Codec) packHuffman(out []byte, deltas []uint64) []byte {
	if len(deltas) == 0 {
		return nil
	}
	symbols := make([]byte, 0, len(deltas))
	for _, delta := range deltas {
		symbols = binary.AppendUvarint(symbols, delta)
	}
	c.huffman.Reuse = huff0.ReusePolicyNone // a block that borrows another block's table is not a block
	compressed, _, err := huff0.Compress1X(symbols, &c.huffman)
	if err != nil {
		return nil
	}
	return append(out, compressed...)
}

func packIntegers(out []byte, values []uint64) []byte {
	for len(values) > 0 {
		for selector, capacity := range wordCounts {
			width := wordWidths[selector]
			n := min(capacity, len(values))
			fits := width != 0 || len(values) >= capacity
			for _, v := range values[:n] {
				if width == 0 {
					fits = fits && v == 1
				} else {
					fits = fits && v < uint64(1)<<width
				}
			}
			if !fits {
				continue
			}
			word := uint64(selector) << 60
			if width > 0 {
				for i, v := range values[:n] {
					word |= v << (uint(i) * uint(width))
				}
			}
			out = binary.LittleEndian.AppendUint64(out, word)
			values = values[n:]
			break
		}
	}
	return out
}

// seedInteger turns the head's first value into the integer the deltas walk from
func (it *Iterator) seedInteger() error {
	if it.valueMode == valueScaled {
		scaled := math.Round(it.head.First * pow10[it.scale])
		if scaled < -0x1p63 || scaled >= 0x1p63 {
			return fmt.Errorf("%w: first scaled value", ErrInvalid)
		}
		it.integer = int64(scaled)
		if math.Float64bits(float64(it.integer)/pow10[it.scale]) != math.Float64bits(it.head.First) {
			return fmt.Errorf("%w: first scaled value", ErrInvalid)
		}
		return nil
	}
	if math.IsNaN(it.head.First) || it.head.First < -0x1p63 || it.head.First >= 0x1p63 {
		return fmt.Errorf("%w: first integer", ErrInvalid)
	}
	it.integer = int64(it.head.First)
	if math.Float64bits(float64(it.integer)) != math.Float64bits(it.head.First) {
		return fmt.Errorf("%w: first integer", ErrInvalid)
	}
	return nil
}

// a scaled value is not integral, so only the checksum stands between a
// corrupted word and a wrong number
func (it *Iterator) fromInteger() error {
	if it.valueMode == valueScaled {
		it.value = math.Float64bits(float64(it.integer) / pow10[it.scale])
		return nil
	}
	converted := float64(it.integer)
	if converted >= 0x1p63 || int64(converted) != it.integer {
		return fmt.Errorf("%w: inexact integer", ErrInvalid)
	}
	it.value = math.Float64bits(converted)
	return nil
}

func (it *Iterator) nextInteger() error {
	if it.deltas == deltasInHuffman {
		zigzag, err := takeUnsigned(&it.values)
		if err != nil {
			return err
		}
		return it.addDelta(zigzag)
	}
	if it.wordLeft == 0 {
		if it.word != 0 {
			return fmt.Errorf("%w: integer word padding", ErrInvalid)
		}
		if len(it.values) < 8 {
			return fmt.Errorf("%w: truncated integer word", ErrInvalid)
		}
		word := binary.LittleEndian.Uint64(it.values)
		it.values = it.values[8:]
		selector := word >> 60
		it.word, it.wordLeft, it.wordBits = word&(1<<60-1), wordCounts[selector], wordWidths[selector]
		if it.wordBits == 0 && (it.word != 0 || it.wordLeft > it.count-it.index) {
			return fmt.Errorf("%w: integer run length", ErrInvalid)
		}
	}
	zigzag := uint64(1)
	if it.wordBits != 0 {
		zigzag = it.word & (uint64(1)<<it.wordBits - 1)
		it.word >>= it.wordBits
	}
	it.wordLeft--
	return it.addDelta(zigzag)
}

func (it *Iterator) addDelta(zigzag uint64) error {
	delta := int64(zigzag>>1) ^ -int64(zigzag&1)
	if (delta > 0 && it.integer > math.MaxInt64-delta) || (delta < 0 && it.integer < math.MinInt64-delta) {
		return fmt.Errorf("%w: integer overflow", ErrInvalid)
	}
	it.integer += delta
	return it.fromInteger()
}

func encodeXOR(samples []Sample) []byte {
	previous := math.Float64bits(samples[0].Value)
	var writer bitWriter
	var leading, trailing uint8
	window := false
	for _, s := range samples[1:] {
		current := math.Float64bits(s.Value)
		difference := current ^ previous
		if difference == 0 {
			writer.put(0, 1)
		} else {
			writer.put(1, 1)
			l := uint8(min(bits.LeadingZeros64(difference), 31)) //nolint:gosec // clamped to 0..31
			r := uint8(bits.TrailingZeros64(difference))         //nolint:gosec // at most 63 when nonzero
			if window && l >= leading && r >= trailing {
				writer.put(0, 1)
				writer.put(difference>>trailing, 64-leading-trailing)
			} else {
				writer.put(1, 1)
				writer.put(uint64(l), 5)
				width := 64 - l - r
				writer.put(uint64(width)&63, 6)
				writer.put(difference>>r, width)
				leading, trailing, window = l, r, true
			}
		}
		previous = current
	}
	return writer.data
}

func (it *Iterator) nextValue() error {
	switch it.valueMode {
	case valueRaw:
		if len(it.values) < 8 {
			return fmt.Errorf("%w: truncated raw value", ErrInvalid)
		}
		it.value = binary.LittleEndian.Uint64(it.values)
		it.values = it.values[8:]
	case valueInteger, valueScaled:
		return it.nextInteger()
	case valueXOR:
		if it.xor.get(1) != 0 {
			if it.xor.get(1) != 0 {
				it.leading = uint8(it.xor.get(5)) //nolint:gosec // get returns at most five bits
				width := uint8(it.xor.get(6))     //nolint:gosec // get returns at most six bits
				if width == 0 {
					width = 64
				}
				if int(it.leading)+int(width) > 64 {
					return fmt.Errorf("%w: XOR window", ErrInvalid)
				}
				it.trailing, it.window = 64-it.leading-width, true
			} else if !it.window {
				return fmt.Errorf("%w: absent XOR window", ErrInvalid)
			}
			it.value ^= it.xor.get(64-it.leading-it.trailing) << it.trailing
		}
		if it.xor.failed {
			return fmt.Errorf("%w: truncated XOR bits", ErrInvalid)
		}
	}
	return nil
}
