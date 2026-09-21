package codec

import (
	"encoding/binary"
	"fmt"
	"math"
	"math/bits"
)

const (
	valueRaw byte = iota
	valueConst
	valueInteger
	valueXOR
	valueScaled
)

// nine decimal places is a nanosecond, and a value written finer than that was
// computed rather than written, where a scale cannot win anything
const maxScale = 9

var pow10 = [maxScale + 1]float64{1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9}

func rawValues(samples []Sample) []byte {
	out := make([]byte, 0, 8*len(samples))
	for _, s := range samples {
		out = binary.LittleEndian.AppendUint64(out, math.Float64bits(s.Value))
	}
	return out
}

func encodeValues(samples []Sample) (byte, []byte) {
	first := math.Float64bits(samples[0].Value)
	constant := true
	for _, s := range samples[1:] {
		constant = constant && math.Float64bits(s.Value) == first
	}
	if constant {
		return valueConst, binary.LittleEndian.AppendUint64(nil, first)
	}
	mode, best := valueRaw, rawValues(samples)
	if integer := encodeIntegers(samples); integer != nil && len(integer) < len(best) {
		mode, best = valueInteger, integer
	}
	if scaled := encodeScaled(samples); scaled != nil && len(scaled) < len(best) {
		mode, best = valueScaled, scaled
	}
	if xor := encodeXOR(samples); len(xor) < len(best) {
		mode, best = valueXOR, xor
	}
	return mode, best
}

// encodeScaled turns decimals into the integers they were written as, and
// refuses the block unless the decoder's own expression returns the original
// bits for every sample.
func encodeScaled(samples []Sample) []byte {
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
	var first, previous int64
	for i, s := range samples {
		scaled := math.Round(s.Value * factor)
		if scaled < -0x1p63 || scaled >= 0x1p63 {
			return nil
		}
		current := int64(scaled)
		if math.Float64bits(float64(current)/factor) != math.Float64bits(s.Value) {
			return nil
		}
		if i == 0 {
			first = current
		} else {
			delta := current - previous
			if (current > previous && delta < 0) || (current < previous && delta > 0) {
				return nil
			}
			zigzag := uint64(delta)<<1 ^ uint64(delta>>63) //nolint:gosec // zigzag deliberately folds the signed bit pattern
			if zigzag >= 1<<60 {
				return nil
			}
			deltas = append(deltas, zigzag)
		}
		previous = current
	}
	out := binary.LittleEndian.AppendUint64([]byte{byte(scale)}, uint64(first)) //nolint:gosec // the signed bit pattern is what travels
	return packIntegers(out, deltas)
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

func encodeIntegers(samples []Sample) []byte {
	deltas := make([]uint64, 0, len(samples)-1)
	var first, previous int64
	for i, s := range samples {
		if math.IsNaN(s.Value) || s.Value < -0x1p63 || s.Value >= 0x1p63 {
			return nil
		}
		current := int64(s.Value)
		if math.Float64bits(float64(current)) != math.Float64bits(s.Value) {
			return nil
		}
		if i == 0 {
			first = current
		} else {
			delta := current - previous
			if (current > previous && delta < 0) || (current < previous && delta > 0) {
				return nil
			}
			zigzag := uint64(delta)<<1 ^ uint64(delta>>63) //nolint:gosec // zigzag deliberately folds the signed bit pattern
			if zigzag >= 1<<60 {
				return nil
			}
			deltas = append(deltas, zigzag)
		}
		previous = current
	}
	return packIntegers(binary.LittleEndian.AppendUint64(nil, uint64(first)), deltas)
}

var (
	wordCounts = [...]int{240, 120, 60, 30, 20, 15, 12, 10, 8, 7, 6, 5, 4, 3, 2, 1}
	wordWidths = [...]uint8{0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 15, 20, 30, 60}
)

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
	delta := int64(zigzag>>1) ^ -int64(zigzag&1)
	if (delta > 0 && it.integer > math.MaxInt64-delta) || (delta < 0 && it.integer < math.MinInt64-delta) {
		return fmt.Errorf("%w: integer overflow", ErrInvalid)
	}
	it.integer += delta
	return it.fromInteger()
}

func encodeXOR(samples []Sample) []byte {
	previous := math.Float64bits(samples[0].Value)
	writer := bitWriter{data: binary.LittleEndian.AppendUint64(nil, previous)}
	var leading, trailing uint8
	window := false
	for _, s := range samples[1:] {
		current := math.Float64bits(s.Value)
		difference := current ^ previous
		if difference == 0 {
			writer.put(0, 1)
		} else {
			writer.put(1, 1)
			l := uint8(min(bits.LeadingZeros64(difference), 31)) //nolint:gosec // leading zeros are clamped to 0..31
			r := uint8(bits.TrailingZeros64(difference))         //nolint:gosec // a nonzero uint64 has at most 63 trailing zeros
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
