package metrics

import (
	"encoding/binary"
	"fmt"
	"math"

	"github.com/tinyshed/tinystore/codec"
)

// changeValues writes where a block's value changes and by how much, keeping
// the shortest of whole numbers, hundredths and raw bits:
//
//	5 5 5 7 7 9    whole numbers    at 3 by +2, 2 later by +2    01 00 03 04 02 04
func changeValues(points []Sample) []byte {
	if constantValues(points) {
		return nil
	}
	var best []byte
	for kind := range byte(3) {
		candidate := changeCandidate(points, kind)
		if candidate != nil && (best == nil || len(candidate) < len(best)) {
			best = candidate
		}
	}
	return best
}

func constantValues(points []Sample) bool {
	for _, point := range points[1:] {
		if math.Float64bits(point.Value) != math.Float64bits(points[0].Value) {
			return false
		}
	}
	return true
}

func changeCandidate(points []Sample, kind byte) []byte {
	integers, ok := changeIntegers(points, kind)
	if !ok {
		return nil
	}
	out := []byte{valuesChanges, kind}
	previous := 0
	for i := 1; i < len(points); i++ {
		if math.Float64bits(points[i].Value) == math.Float64bits(points[i-1].Value) {
			continue
		}
		out = appendCount(out, i-previous)
		previous = i
		if kind == 2 {
			out = binary.LittleEndian.AppendUint64(out, math.Float64bits(points[i].Value))
		} else {
			out = binary.AppendVarint(out, integers[i]-integers[i-1])
		}
	}
	return out
}

// changeIntegers are the values counted in the kind's unit, when every one
// divides back to its exact bits; raw bits need none.
func changeIntegers(points []Sample, kind byte) ([]int64, bool) {
	integers := make([]int64, len(points))
	if kind == 2 {
		return integers, true
	}
	factor := changeFactor(kind)
	for i, point := range points {
		q := math.Round(point.Value * factor)
		if math.IsNaN(q) || math.IsInf(q, 0) || math.Abs(q) >= 0x1p60 {
			return nil, false
		}
		integers[i] = int64(q)
		if math.Float64bits(float64(integers[i])/factor) != math.Float64bits(point.Value) {
			return nil, false
		}
	}
	return integers, true
}

func changeFactor(kind byte) float64 {
	if kind == 1 {
		return 100
	}
	return 1
}

func readChanges(head codec.Head, body []byte) ([]Sample, error) {
	r := binaryReader{data: body}
	kind := r.byte()
	if kind > 2 {
		return nil, fmt.Errorf("%w: change representation", ErrCorrupt)
	}
	value, integer, err := firstChange(head.First, kind)
	if err != nil {
		return nil, err
	}

	out := make([]Sample, head.Count)
	next := r.size(head.Count - 1)
	if next == 0 || r.err != nil {
		return nil, fmt.Errorf("%w: first change position", ErrCorrupt)
	}
	for i := range out {
		if i == next {
			if value, integer, err = readChange(&r, kind, integer); err != nil {
				return nil, err
			}
			if next, err = nextChange(&r, head.Count, i); err != nil {
				return nil, err
			}
		}
		out[i] = Sample{At: int64(i), Value: value}
	}
	if err = r.finish(); err != nil {
		return nil, err
	}
	return out, nil
}

// firstChange checks that the first value is one the kind could have written.
func firstChange(first float64, kind byte) (float64, int64, error) {
	if kind == 2 {
		return first, 0, nil
	}
	factor := changeFactor(kind)
	q := math.Round(first * factor)
	if math.IsNaN(q) || math.IsInf(q, 0) || math.Abs(q) >= 0x1p60 ||
		math.Float64bits(float64(int64(q))/factor) != math.Float64bits(first) {
		return 0, 0, fmt.Errorf("%w: first change value", ErrCorrupt)
	}
	return first, int64(q), nil
}

// readChange reads the next value: raw bits, or a delta to the running integer
// that must stay inside the range the encoder accepts.
func readChange(r *binaryReader, kind byte, integer int64) (float64, int64, error) {
	if kind == 2 {
		return math.Float64frombits(r.word()), integer, nil
	}
	delta := unfoldSigned(r.unsigned())
	if (delta > 0 && integer > math.MaxInt64-delta) || (delta < 0 && integer < math.MinInt64-delta) {
		return 0, 0, fmt.Errorf("%w: change integer overflow", ErrCorrupt)
	}
	integer += delta
	if integer <= -(1<<60) || integer >= 1<<60 {
		return 0, 0, fmt.Errorf("%w: change integer range", ErrCorrupt)
	}
	return float64(integer) / changeFactor(kind), integer, nil
}

// nextChange is the position of the change after i, or the end of the block
// when the body holds no more.
func nextChange(r *binaryReader, count, i int) (int, error) {
	if len(r.data) == 0 {
		return count, nil
	}
	gap := r.size(count - 1 - i)
	if gap == 0 || r.err != nil {
		return 0, fmt.Errorf("%w: change position", ErrCorrupt)
	}
	return i + gap, nil
}
