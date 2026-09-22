//nolint:gosec // bit widths and residual counts are validated before indexing or integer conversion
package metrics

import (
	"encoding/binary"
	"fmt"
	"math/bits"
)

const (
	residualZero byte = iota
	residualSigns
	residualBitmap
	residualPacked
	residualSparse
	residualVarint
	residualCompressed
)

func packedBits(values []uint64, width int) []byte {
	out := make([]byte, (len(values)*width+7)/8)
	for i, value := range values {
		for bit := range width {
			position := i*width + bit
			out[position/8] |= byte(value>>uint(bit)&1) << uint(position%8)
		}
	}
	return out
}

func readPackedBits(data []byte, count, width int) ([]uint64, error) {
	if width < 0 || width > 64 || count < 0 || count > blockSamples || len(data) != (count*width+7)/8 {
		return nil, fmt.Errorf("%w: packed residual size", ErrCorrupt)
	}
	if used := count * width % 8; used != 0 && data[len(data)-1]>>uint(used) != 0 {
		return nil, fmt.Errorf("%w: residual padding", ErrCorrupt)
	}
	out := make([]uint64, count)
	for i := range out {
		for bit := range width {
			position := i*width + bit
			out[i] |= uint64(data[position/8]>>uint(position%8)&1) << uint(bit)
		}
	}
	return out, nil
}

func (m *metadataCodec) encodeResiduals(residuals []int64) []byte {
	values := make([]uint64, len(residuals))
	var nonzero []uint64
	var positions []int
	var union uint64
	for i, value := range residuals {
		values[i] = foldSigned(value)
		union |= values[i]
		if value != 0 {
			nonzero = append(nonzero, values[i])
			positions = append(positions, i)
		}
	}
	if len(nonzero) == 0 {
		return []byte{residualZero}
	}
	best := []byte{residualVarint}
	for _, value := range residuals {
		best = binary.AppendVarint(best, value)
	}
	consider := func(candidate []byte) {
		if len(candidate) < len(best) {
			best = candidate
		}
	}
	compressed := m.encode(best[1:])
	if compressed[0] == 1 {
		consider(append([]byte{residualCompressed}, compressed[1:]...))
	}
	width := bits.Len64(union)
	consider(append([]byte{residualPacked, byte(width)}, packedBits(values, width)...))
	bitmap := make([]byte, (len(values)+7)/8)
	for _, i := range positions {
		bitmap[i/8] |= 1 << uint(i%8)
	}
	consider(append(append([]byte{residualBitmap, byte(width)}, bitmap...), packedBits(nonzero, width)...))
	if union <= 3 {
		signs := make([]uint64, len(nonzero))
		onlySigns := true
		for i, p := range positions {
			if residuals[p] != 1 && residuals[p] != -1 {
				onlySigns = false
				break
			}
			if residuals[p] < 0 {
				signs[i] = 1
			}
		}
		if onlySigns {
			consider(append(append([]byte{residualSigns}, bitmap...), packedBits(signs, 1)...))
		}
	}
	sparse := []byte{residualSparse}
	previous := -1
	for _, i := range positions {
		sparse = binary.AppendUvarint(sparse, uint64(i-previous))
		sparse = binary.AppendUvarint(sparse, values[i])
		previous = i
	}
	consider(sparse)
	return best
}

func (m *metadataCodec) decodeResiduals(data []byte, count int) ([]int64, error) {
	if count < 0 || count >= blockSamples || len(data) == 0 || len(data) > maxDirectoryBytes {
		return nil, fmt.Errorf("%w: residual bounds", ErrCorrupt)
	}
	r := binaryReader{data: data}
	mode := r.byte()
	out := make([]int64, count)
	if mode == residualCompressed {
		plain, err := m.decode(append([]byte{1}, r.data...))
		if err != nil {
			return nil, err
		}
		r.data = plain
		mode = residualVarint
	}
	switch mode {
	case residualZero:
	case residualVarint:
		for i := range out {
			out[i] = unfoldSigned(r.unsigned())
		}
	case residualSparse:
		position := -1
		for len(r.data) > 0 && r.err == nil {
			gap := r.size(count - 1 - position)
			if gap == 0 {
				return nil, fmt.Errorf("%w: residual position", ErrCorrupt)
			}
			position += gap
			out[position] = unfoldSigned(r.unsigned())
		}
	case residualPacked:
		width := int(r.byte())
		values, err := readPackedBits(r.data, count, width)
		if err != nil {
			return nil, err
		}
		r.data = nil
		for i, value := range values {
			out[i] = unfoldSigned(value)
		}
	case residualSigns, residualBitmap:
		width := 1
		if mode == residualBitmap {
			width = int(r.byte())
		}
		bitmap := r.take((count + 7) / 8)
		if r.err != nil {
			return nil, r.err
		}
		if used := count % 8; used != 0 && bitmap[len(bitmap)-1]>>uint(used) != 0 {
			return nil, fmt.Errorf("%w: residual bitmap padding", ErrCorrupt)
		}
		nonzero := 0
		for _, value := range bitmap {
			nonzero += bits.OnesCount8(value)
		}
		values, err := readPackedBits(r.data, nonzero, width)
		if err != nil {
			return nil, err
		}
		r.data = nil
		position := 0
		for i := range out {
			if bitmap[i/8]&(1<<uint(i%8)) == 0 {
				continue
			}
			value := values[position]
			position++
			if mode == residualSigns {
				out[i] = 1
				if value == 1 {
					out[i] = -1
				}
			} else {
				out[i] = unfoldSigned(value)
			}
		}
	default:
		return nil, fmt.Errorf("%w: residual representation", ErrCorrupt)
	}
	if err := r.finish(); err != nil {
		return nil, err
	}
	return out, nil
}
