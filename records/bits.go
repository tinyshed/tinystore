package records

import "encoding/binary"

// bitWriter appends values least significant bit first, 64 bits at a time
type bitWriter struct {
	out  []byte
	acc  uint64
	used uint
}

func (w *bitWriter) write(value uint64, width uint) {
	for width > 0 {
		take := min(width, 64-w.used)
		part := value
		if take < 64 {
			part &= 1<<take - 1
		}
		w.acc |= part << w.used
		w.used += take
		value >>= take
		width -= take
		if w.used == 64 {
			w.out = binary.LittleEndian.AppendUint64(w.out, w.acc)
			w.acc, w.used = 0, 0
		}
	}
}

// finish writes the bits still held, padded to a whole byte
func (w *bitWriter) finish() []byte {
	for w.used > 0 {
		w.out = append(w.out, byte(w.acc)) //nolint:gosec // the accumulator's low byte, the next to write
		w.acc >>= 8
		w.used -= min(w.used, 8)
	}
	return w.out
}

// bitReader reads what bitWriter wrote; a read past the end sets short
type bitReader struct {
	data  []byte
	at    uint64
	short bool
}

func (r *bitReader) read(width uint) uint64 {
	var value uint64
	for got := uint(0); got < width; {
		index := r.at / 8
		if index >= uint64(len(r.data)) {
			r.short = true
			return 0
		}
		shift := uint(r.at % 8)
		take := min(8-shift, width-got)
		value |= (uint64(r.data[index]>>shift) & (1<<take - 1)) << got
		got += take
		r.at += uint64(take)
	}
	return value
}

// consumed is how many whole bytes the reads so far have touched
func (r *bitReader) consumed() uint64 {
	return (r.at + 7) / 8
}

// a rice code writes value>>k in unary and the low k bits as they are; a
// quotient of riceEscape or more is written as the escape and 64 raw bits
//
//	k=2   5 → 1 0 01     0 → 0 00     13 → 111 0 01
const riceEscape = 32

func writeRice(w *bitWriter, value uint64, k uint) {
	if quotient := value >> k; quotient < riceEscape {
		w.write(1<<quotient-1, uint(quotient))
		w.write(0, 1)
		w.write(value, k)
		return
	}
	w.write(1<<riceEscape-1, riceEscape)
	w.write(value, 64)
}

func readRice(r *bitReader, k uint) uint64 {
	quotient := uint64(0)
	for quotient < riceEscape && r.read(1) == 1 {
		quotient++
	}
	if quotient == riceEscape {
		return r.read(64)
	}
	return quotient<<k | r.read(k)
}

func riceBits(value uint64, k uint) uint64 {
	if quotient := value >> k; quotient < riceEscape {
		return quotient + 1 + uint64(k)
	}
	return riceEscape + 64
}

// distance is end - start as an unsigned number, exact across the whole signed range
func distance(start, end int64) uint64 {
	return uint64(end) - uint64(start) //nolint:gosec // modular subtraction is the exact distance
}

// advance is start + span, wrapping exactly as distance does, so it inverts it
func advance(start int64, span uint64) int64 {
	return int64(uint64(start) + span) //nolint:gosec // the inverse of distance, wrapping alike
}

// unsigned is a count or a length, which is never negative
func unsigned(n int) uint64 {
	return uint64(n) //nolint:gosec // counts and lengths are never negative
}

// parameter is a packer's width, group or rice parameter, each at most 64
func parameter(value uint) byte {
	return byte(min(value, 64))
}

// zigzag folds the sign into the lowest bit, as a varint does: small
// magnitudes of either sign become small numbers
func zigzag(value int64) uint64 {
	return uint64(value<<1) ^ uint64(value>>63) //nolint:gosec // a bit transform, not a value converted
}
