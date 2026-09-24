//nolint:gosec // unsigned conversions preserve IEEE bits; stream counts and scales are checked before allocation
package metrics

import (
	"encoding/binary"
	"fmt"
	"math"

	"github.com/tinyshed/tinystore/codec"
)

func changeValues(points []Sample) []byte {
	constant := true
	for _, p := range points[1:] {
		constant = constant && math.Float64bits(p.Value) == math.Float64bits(points[0].Value)
	}
	if constant {
		return nil
	}
	var best []byte
	for kind := range byte(3) {
		factor := 1.0
		if kind == 1 {
			factor = 100
		}
		integers := make([]int64, len(points))
		valid := true
		if kind < 2 {
			for i, p := range points {
				q := math.Round(p.Value * factor)
				if math.IsNaN(q) || math.IsInf(q, 0) || math.Abs(q) >= 0x1p60 {
					valid = false
					break
				}
				integers[i] = int64(q)
				if math.Float64bits(float64(integers[i])/factor) != math.Float64bits(p.Value) {
					valid = false
					break
				}
			}
		}
		if !valid {
			continue
		}
		out := []byte{valuesChanges, kind}
		previous := 0
		for i := 1; i < len(points); i++ {
			if math.Float64bits(points[i].Value) == math.Float64bits(points[i-1].Value) {
				continue
			}
			out = binary.AppendUvarint(out, uint64(i-previous))
			previous = i
			if kind == 2 {
				out = binary.LittleEndian.AppendUint64(out, math.Float64bits(points[i].Value))
			} else {
				out = binary.AppendVarint(out, integers[i]-integers[i-1])
			}
		}
		if best == nil || len(out) < len(best) {
			best = out
		}
	}
	return best
}

func readChanges(head codec.Head, body []byte) ([]Sample, error) {
	r := binaryReader{data: body}
	kind := r.byte()
	if kind > 2 {
		return nil, fmt.Errorf("%w: change representation", ErrCorrupt)
	}
	factor := 1.0
	if kind == 1 {
		factor = 100
	}
	value := head.First
	integer := int64(0)
	if kind < 2 {
		q := math.Round(value * factor)
		if math.IsNaN(q) || math.IsInf(q, 0) || math.Abs(q) >= 0x1p60 || math.Float64bits(float64(int64(q))/factor) != math.Float64bits(value) {
			return nil, fmt.Errorf("%w: first change value", ErrCorrupt)
		}
		integer = int64(q)
	}
	out := make([]Sample, head.Count)
	next := r.size(head.Count - 1)
	if next == 0 || r.err != nil {
		return nil, fmt.Errorf("%w: first change position", ErrCorrupt)
	}
	for i := range out {
		if i == next {
			if kind == 2 {
				value = math.Float64frombits(r.word())
			} else {
				delta := unfoldSigned(r.unsigned())
				if (delta > 0 && integer > math.MaxInt64-delta) || (delta < 0 && integer < math.MinInt64-delta) {
					return nil, fmt.Errorf("%w: change integer overflow", ErrCorrupt)
				}
				integer += delta
				if integer <= -(1<<60) || integer >= 1<<60 {
					return nil, fmt.Errorf("%w: change integer range", ErrCorrupt)
				}
				value = float64(integer) / factor
			}
			if len(r.data) == 0 {
				next = head.Count
			} else {
				gap := r.size(head.Count - 1 - i)
				if gap == 0 || r.err != nil {
					return nil, fmt.Errorf("%w: change position", ErrCorrupt)
				}
				next = i + gap
			}
		}
		out[i] = Sample{At: int64(i), Value: value}
	}
	if err := r.finish(); err != nil {
		return nil, err
	}
	return out, nil
}
