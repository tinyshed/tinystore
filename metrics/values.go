//nolint:gosec // unsigned conversions preserve IEEE bits; stream counts and scales are checked before allocation
package metrics

import (
	"encoding/binary"
	"fmt"
	"hash/crc32"
	"math"

	"github.com/tinyshed/tinystore/codec"
)

const (
	valuesOrdinary byte = iota
	valuesChanges
	valuesGrid
)

// ordinaryValues reuses the existing value codec without storing an extra timestamp envelope
func (s *Store) ordinaryValues(points []Sample) ([]byte, error) {
	flat := make([]Sample, len(points))
	for i, p := range points {
		flat[i] = Sample{At: int64(i), Value: p.Value}
	}
	_, body, err := s.encoder.Encode(flat)
	if err != nil {
		return nil, fmt.Errorf("encode ordinary values: %w", err)
	}
	return append([]byte{valuesOrdinary, body[1]}, body[4:len(body)-4]...), nil
}

func (s *Store) readOrdinary(head codec.Head, body []byte) ([]Sample, error) {
	if len(body) < 2 || body[0] != valuesOrdinary || body[1]&3 != 0 {
		return nil, fmt.Errorf("%w: ordinary value flags", ErrCorrupt)
	}
	head.Start = 0
	head.End = int64(head.Count - 1)
	encoded := append([]byte{1, body[1], 0, 0}, body[2:]...)
	key := binary.LittleEndian.AppendUint64(nil, 0)
	key = binary.LittleEndian.AppendUint64(key, uint64(head.End))
	key = binary.LittleEndian.AppendUint16(key, uint16(head.Count))
	key = binary.LittleEndian.AppendUint64(key, math.Float64bits(head.First))
	table := crc32.MakeTable(crc32.Castagnoli)
	sum := crc32.Update(crc32.Checksum(key, table), table, encoded)
	encoded = binary.LittleEndian.AppendUint32(encoded, sum)
	iterator, err := s.decoder.Decode(head, encoded)
	if err != nil {
		return nil, fmt.Errorf("%w: ordinary values: %w", ErrCorrupt, err)
	}
	out := make([]Sample, 0, head.Count)
	for iterator.Next() {
		out = append(out, iterator.Sample())
	}
	if err = iterator.Err(); err != nil {
		return nil, fmt.Errorf("%w: ordinary value stream: %w", ErrCorrupt, err)
	}
	return out, nil
}

func changeValues(points []Sample) []byte {
	constant := true
	for _, p := range points[1:] {
		constant = constant && math.Float64bits(p.Value) == math.Float64bits(points[0].Value)
	}
	if constant {
		return nil
	}
	var best []byte
	for kind := byte(0); kind < 3; kind++ {
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

// the first block chooses a scale; later blocks try only that hint and can always decline it
func (s *Store) encodeValues(points []Sample, hint int) ([]byte, int, error) {
	changes := changeValues(points)
	if len(changes) == 0 {
		return nil, hint, nil
	}
	best, err := s.ordinaryValues(points)
	if err != nil {
		return nil, hint, err
	}
	if len(changes) < len(best) {
		best = changes
	}
	selected := hint
	if hint == -2 {
		selected = -1
	}
	if hint >= 0 || hint == -2 {
		first, last := hint, hint
		if hint == -2 {
			first, last = 0, 15
		}
		for scale := first; scale <= last; scale++ {
			candidate, gridErr := s.gridValues(points, scale)
			if gridErr != nil {
				return nil, hint, gridErr
			}
			if len(candidate) > 0 && len(candidate) < len(best) {
				best = candidate
				selected = scale
			}
		}
	}
	return best, selected, nil
}

func valueChecksum(block storedBlock, body []byte) uint32 {
	key := binary.LittleEndian.AppendUint64(nil, uint64(block.head.Start))
	key = binary.LittleEndian.AppendUint64(key, uint64(block.head.End))
	key = binary.LittleEndian.AppendUint16(key, uint16(block.head.Count))
	key = binary.LittleEndian.AppendUint64(key, math.Float64bits(block.head.First))
	sum := crc32.Update(crc32.ChecksumIEEE(key), crc32.IEEETable, block.clock)
	return crc32.Update(sum, crc32.IEEETable, body)
}

func sealValueBody(block storedBlock, body []byte) []byte {
	if len(body) == 0 {
		return nil
	}
	return binary.LittleEndian.AppendUint32(body, valueChecksum(block, body))
}

func (s *Store) decodeBlock(block storedBlock) ([]Sample, error) {
	if block.format < 2 {
		iterator, err := s.decoder.Decode(block.head, block.body)
		if err != nil {
			return nil, fmt.Errorf("%w: legacy block: %w", ErrCorrupt, err)
		}
		out := make([]Sample, 0, block.head.Count)
		for iterator.Next() {
			out = append(out, iterator.Sample())
		}
		if err = iterator.Err(); err != nil {
			return nil, fmt.Errorf("%w: legacy values: %w", ErrCorrupt, err)
		}
		return out, nil
	}
	times, err := decodeClockValues(block)
	if err != nil {
		return nil, err
	}
	head := block.head
	var out []Sample
	if len(block.body) == 0 {
		out = make([]Sample, head.Count)
		for i := range out {
			out[i].Value = head.First
		}
	} else {
		if len(block.body) < 5 {
			return nil, fmt.Errorf("%w: value body size", ErrCorrupt)
		}
		body := block.body[:len(block.body)-4]
		if valueChecksum(block, body) != binary.LittleEndian.Uint32(block.body[len(block.body)-4:]) {
			return nil, fmt.Errorf("%w: value body checksum", ErrCorrupt)
		}
		switch body[0] {
		case valuesOrdinary:
			out, err = s.readOrdinary(head, body)
		case valuesChanges:
			out, err = readChanges(head, body[1:])
		case valuesGrid:
			r := binaryReader{data: body[1:]}
			scale := int(r.byte())
			if scale > 15 {
				return nil, fmt.Errorf("%w: grid scale", ErrCorrupt)
			}
			n := r.size(maxPayloadBytes)
			base := r.take(n)
			if r.err != nil {
				return nil, r.err
			}
			factor := math.Pow10(scale)
			originalFirst := head.First
			head.First = math.Round(head.First * factor)
			if math.IsNaN(head.First) || math.IsInf(head.First, 0) || math.Abs(head.First) > 0x1p53 {
				return nil, fmt.Errorf("%w: grid seed", ErrCorrupt)
			}
			out, err = s.readOrdinary(head, base)
			if err != nil {
				return nil, err
			}
			residuals, resErr := s.metadata.decodeResiduals(r.data, head.Count-1)
			if resErr != nil {
				return nil, resErr
			}
			out[0].Value = originalFirst
			for i := 1; i < len(out); i++ {
				out[i].Value = orderedValue(orderedBits(out[i].Value/factor) + uint64(residuals[i-1]))
			}
		default:
			return nil, fmt.Errorf("%w: value representation", ErrCorrupt)
		}
		if err != nil {
			return nil, err
		}
	}
	if len(out) != len(times) {
		return nil, fmt.Errorf("%w: value count", ErrCorrupt)
	}
	for i := range out {
		out[i].At = times[i]
	}
	return out, nil
}
