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
