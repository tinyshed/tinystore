package spike

import (
	"encoding/binary"
	"math/bits"
)

func recordZigzag(value uint64) uint64 {
	return value<<1 ^ uint64(int64(value)>>63)
}

func recordUnzigzag(value uint64) uint64 {
	return value>>1 ^ -(value & 1)
}

func recordVarints(values []uint64) []byte {
	var out []byte
	for _, value := range values {
		out = binary.AppendUvarint(out, value)
	}
	return out
}

func recordPackedNumbers(values []uint64) []byte {
	minimum, maximum := ^uint64(0), uint64(0)
	for _, value := range values {
		minimum, maximum = min(minimum, value), max(maximum, value)
	}
	if len(values) == 0 {
		minimum = 0
	}
	width := bits.Len64(maximum - minimum)
	out := binary.AppendUvarint(nil, minimum)
	out = append(out, byte(width))
	packed := make([]byte, (len(values)*width+7)/8)
	for i, value := range values {
		value -= minimum
		for bit := range width {
			if value>>bit&1 != 0 {
				at := i*width + bit
				packed[at/8] |= 1 << (at % 8)
			}
		}
	}
	return append(out, packed...)
}

func (c *recordBlockCodec) encodeNumbers(values []uint64) []byte {
	absolute := append([]byte{0}, recordVarints(values)...)
	packed := append([]byte{1}, recordPackedNumbers(values)...)
	deltas := make([]uint64, len(values))
	previous := uint64(0)
	for i, value := range values {
		deltas[i] = recordZigzag(value - previous)
		previous = value
	}
	candidates := [][]byte{
		absolute, packed,
		append([]byte{2}, recordVarints(deltas)...), append([]byte{3}, recordPackedNumbers(deltas)...),
	}
	best, cost := absolute, len(c.pack(absolute))
	for _, candidate := range candidates[1:] {
		if size := len(c.pack(candidate)); size < cost {
			best, cost = candidate, size
		}
	}
	if c.features&recordNumberModels != 0 {
		best = c.modelRecordNumbers(values, best)
	}
	return best
}

func readRecordNumbers(cursor *recordCursor, count int) []uint64 {
	mode := cursor.number(7)
	if mode == 6 {
		return readSparseRecordNumbers(cursor, count)
	}
	if mode == 7 {
		return readRadixRecordNumbers(cursor, count)
	}
	if mode >= 4 {
		return readRecordNumberModel(cursor, mode, count)
	}
	values := make([]uint64, count)
	if mode&1 == 0 {
		for i := range values {
			values[i] = cursor.unsigned()
		}
	} else {
		minimum, width := cursor.unsigned(), cursor.number(64)
		packed := cursor.take((count*width + 7) / 8)
		if cursor.err != nil {
			return values
		}
		for i := range values {
			for bit := range width {
				at := i*width + bit
				values[i] |= uint64(packed[at/8]>>(at%8)&1) << bit
			}
			if values[i] > ^uint64(0)-minimum {
				cursor.fail("packed number overflow")
			}
			values[i] += minimum
		}
		if used := count * width % 8; used != 0 && packed[len(packed)-1]>>used != 0 {
			cursor.fail("packed number padding")
		}
	}
	if mode >= 2 {
		previous := uint64(0)
		for i, value := range values {
			previous += recordUnzigzag(value)
			values[i] = previous
		}
	}
	return values
}
