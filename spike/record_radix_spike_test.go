package spike

import (
	"encoding/binary"
	"slices"
)

func (c *recordBlockCodec) sparseRecordNumbers(values []uint64) []byte {
	counts := map[uint64]int{}
	base, frequency := uint64(0), 0
	for _, value := range values {
		counts[value]++
		if counts[value] > frequency {
			base, frequency = value, counts[value]
		}
	}
	if frequency*2 < len(values) {
		return nil
	}
	var positions, exceptions []uint64
	for i, value := range values {
		if value != base {
			positions = append(positions, uint64(i))
			exceptions = append(exceptions, value)
		}
	}
	out := binary.AppendUvarint([]byte{6}, base)
	out = binary.AppendUvarint(out, uint64(len(positions)))
	out = append(out, c.encodeNumbers(positions)...)
	return append(out, c.encodeNumbers(exceptions)...)
}

func readOrdinaryRecordNumbers(cursor *recordCursor, count int) []uint64 {
	if len(cursor.data) == 0 || cursor.data[0] > 3 {
		cursor.fail("nested numeric transform")
		return nil
	}
	return readRecordNumbers(cursor, count)
}

func readSparseRecordNumbers(cursor *recordCursor, count int) []uint64 {
	base := cursor.unsigned()
	exceptions := cursor.number(count)
	positions := readOrdinaryRecordNumbers(cursor, exceptions)
	values := readOrdinaryRecordNumbers(cursor, exceptions)
	if cursor.err != nil {
		return nil
	}
	out := make([]uint64, count)
	for i := range out {
		out[i] = base
	}
	for i, position := range positions {
		if position >= uint64(count) || (i > 0 && positions[i-1] >= position) {
			cursor.fail("sparse numeric position")
			return nil
		}
		out[position] = values[i]
	}
	return out
}

func recordRadixGroup(base uint64) int {
	if base < 2 {
		return 0
	}
	count, product := 0, uint64(1)
	for count < 64 && product <= ^uint64(0)/base {
		product *= base
		count++
	}
	return count
}

func (c *recordBlockCodec) radixRecordNumbers(values []uint64) []byte {
	minimum, maximum := slices.Min(values), slices.Max(values)
	if maximum-minimum == ^uint64(0) {
		return nil
	}
	base := maximum - minimum + 1
	group := recordRadixGroup(base)
	if group < 2 || base&(base-1) == 0 {
		return nil
	}
	var packed []uint64
	for start := 0; start < len(values); start += group {
		word, multiplier := uint64(0), uint64(1)
		for _, value := range values[start:min(start+group, len(values))] {
			word += (value - minimum) * multiplier
			multiplier *= base
		}
		packed = append(packed, word)
	}
	out := binary.AppendUvarint([]byte{7}, minimum)
	out = binary.AppendUvarint(out, base)
	return append(out, c.encodeNumbers(packed)...)
}

func readRadixRecordNumbers(cursor *recordCursor, count int) []uint64 {
	minimum, base := cursor.unsigned(), cursor.unsigned()
	group := recordRadixGroup(base)
	if group < 2 || base-1 > ^uint64(0)-minimum {
		cursor.fail("numeric radix bounds")
		return nil
	}
	words := readOrdinaryRecordNumbers(cursor, (count+group-1)/group)
	if cursor.err != nil {
		return nil
	}
	values := make([]uint64, count)
	for i, word := range words {
		for index := i * group; index < min((i+1)*group, count); index++ {
			values[index] = minimum + word%base
			word /= base
		}
		if word != 0 {
			cursor.fail("numeric radix padding")
		}
	}
	return values
}
