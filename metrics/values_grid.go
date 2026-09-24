//nolint:gosec // unsigned conversions preserve IEEE bits; stream counts and scales are checked before allocation
package metrics

import (
	"encoding/binary"
	"math"
)

func orderedBits(value float64) uint64 {
	word := math.Float64bits(value)
	if word>>63 != 0 {
		return ^word
	}
	return word ^ (1 << 63)
}

func orderedValue(word uint64) float64 {
	if word>>63 == 0 {
		return math.Float64frombits(^word)
	}
	return math.Float64frombits(word ^ (1 << 63))
}

func (s *Store) gridValues(points []Sample, scale int) ([]byte, error) {
	factor := math.Pow10(scale)
	quantized := make([]Sample, len(points))
	residuals := make([]int64, len(points)-1)
	for i, p := range points {
		q := math.Round(p.Value * factor)
		if math.IsNaN(q) || math.IsInf(q, 0) || math.Abs(q) > 0x1p53 {
			return nil, nil
		}
		quantized[i] = Sample{At: int64(i), Value: q}
		if i > 0 {
			residuals[i-1] = int64(orderedBits(p.Value) - orderedBits(q/factor))
		}
	}
	base, err := s.ordinaryValues(quantized)
	if err != nil {
		return nil, err
	}
	residual := s.metadata.encodeResiduals(residuals)
	out := binary.AppendUvarint([]byte{valuesGrid, byte(scale)}, uint64(len(base)))
	out = append(out, base...)
	return append(out, residual...), nil
}
