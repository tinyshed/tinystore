package metrics

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"math/big"
)

// At most 240 finite float64 values need 2,106 bits in units of 2^-1074.
const (
	exactSummaryBits  = 2106
	exactSummaryBytes = (exactSummaryBits + 7) / 8
)

func encodeExact(value *big.Int) []byte {
	if value.Sign() == 0 {
		return []byte{0}
	}
	var magnitude big.Int
	magnitude.Abs(value)
	exponent := magnitude.TrailingZeroBits()
	magnitude.Rsh(&magnitude, exponent)
	encoded := magnitude.Bytes()
	out := appendCount(nil, len(encoded))
	token := uint64(exponent) << 1
	if value.Sign() < 0 {
		token |= 1
	}
	out = binary.AppendUvarint(out, token)
	return append(out, encoded...)
}

func readExact(reader *binaryReader) []byte {
	before := reader.data
	var value big.Int
	readExactValue(reader, &value)
	if reader.err != nil {
		return nil
	}
	encoded := before[:len(before)-len(reader.data)]
	if !bytes.Equal(encoded, encodeExact(&value)) {
		reader.err = fmt.Errorf("%w: noncanonical exact summary", ErrCorrupt)
		return nil
	}
	return encoded
}

func readExactValue(reader *binaryReader, value *big.Int) {
	length := reader.size(exactSummaryBytes)
	if length == 0 {
		value.SetInt64(0)
		return
	}
	token := reader.unsigned()
	if token>>1 >= exactSummaryBits {
		reader.err = fmt.Errorf("%w: exact summary exponent", ErrCorrupt)
		return
	}
	magnitude := reader.take(length)
	if reader.err != nil {
		return
	}
	if magnitude[0] == 0 || magnitude[len(magnitude)-1]&1 == 0 {
		reader.err = fmt.Errorf("%w: exact summary magnitude", ErrCorrupt)
		return
	}
	value.SetBytes(magnitude)
	exponent := uint(token >> 1)
	if value.BitLen()+int(exponent) > exactSummaryBits {
		reader.err = fmt.Errorf("%w: exact summary magnitude bound", ErrCorrupt)
		return
	}
	value.Lsh(value, exponent)
	if token&1 != 0 {
		value.Neg(value)
	}
}

func exactValue(encoded []byte, value *big.Int) error {
	reader := binaryReader{data: encoded}
	readExactValue(&reader, value)
	return reader.finish()
}

func exactSummarize(points []Sample, kind Kind) blockSummary {
	summary := summarize(points, kind)
	var sum, increase bucketAccumulator
	for _, point := range points {
		if sum.add(point.Value, AggregateSum, kind) != nil {
			return summary
		}
		if kind == Counter && increase.add(point.Value, AggregateIncrease, kind) != nil {
			return summary
		}
	}
	summary.min, summary.max = sum.minimum, sum.maximum
	summary.exactSum = encodeExact(&sum.exact)
	summary.exactIncrease = encodeExact(&increase.exact)
	return summary
}
