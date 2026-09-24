package metrics

import (
	"fmt"
	"math"

	"github.com/tinyshed/tinystore/codec"
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
			residuals[i-1] = int64(orderedBits(p.Value) - orderedBits(q/factor)) //nolint:gosec // read as signed
		}
	}
	base, err := s.ordinaryValues(quantized)
	if err != nil {
		return nil, err
	}
	residual := s.metadata.encodeResiduals(residuals)
	out := []byte{valuesGrid, byte(scale)} //nolint:gosec // scales run from 0 to 15
	out = appendCount(out, len(base))
	out = append(out, base...)
	return append(out, residual...), nil
}

// readGrid decodes the values as integers on the grid, then adds each one's
// residual back in the ordered-bits space, which restores every original bit.
func (s *Store) readGrid(head codec.Head, data []byte) ([]Sample, error) {
	r := binaryReader{data: data}
	scale := int(r.byte())
	if scale > 15 {
		return nil, fmt.Errorf("%w: grid scale", ErrCorrupt)
	}
	base := r.take(r.size(maxPayloadBytes))
	if r.err != nil {
		return nil, r.err
	}

	factor := math.Pow10(scale)
	first := head.First
	head.First = math.Round(head.First * factor)
	if math.IsNaN(head.First) || math.IsInf(head.First, 0) || math.Abs(head.First) > 0x1p53 {
		return nil, fmt.Errorf("%w: grid seed", ErrCorrupt)
	}
	out, err := s.readOrdinary(head, base)
	if err != nil {
		return nil, err
	}

	residuals, err := s.metadata.decodeResiduals(r.data, head.Count-1)
	if err != nil {
		return nil, err
	}
	out[0].Value = first
	for i := 1; i < len(out); i++ {
		out[i].Value = orderedValue(orderedBits(out[i].Value/factor) + uint64(residuals[i-1])) //nolint:gosec // bits
	}
	return out, nil
}
