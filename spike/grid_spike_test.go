package spike

import (
	"encoding/binary"
	"math"
	"sort"
	"testing"
	"time"

	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore/codec"
)

// residual modes, tagged in the first byte of the residual stream
const (
	resZero = iota
	resBitmapSign
	resBitmapPacked
	resPackedAll
	resSparseGap
	resVarint
	resVarintZstd
)

func zigzag(v int64) uint64 { return uint64(v<<1) ^ uint64(v>>63) }

func unzigzag(u uint64) int64 { return int64(u>>1) ^ -int64(u&1) }

func bitWidth(u uint64) int {
	w := 0
	for u != 0 {
		w++
		u >>= 1
	}
	return w
}

func packBits(vals []uint64, w int) []byte {
	if w == 0 {
		return nil
	}
	out := make([]byte, (len(vals)*w+7)/8)
	bit := 0
	for _, v := range vals {
		for i := range w {
			if v>>uint(i)&1 == 1 {
				out[(bit+i)/8] |= 1 << uint((bit+i)%8)
			}
		}
		bit += w
	}
	return out
}

func unpackBits(data []byte, n, w int) []uint64 {
	out := make([]uint64, n)
	if w == 0 {
		return out
	}
	bit := 0
	for i := range out {
		var v uint64
		for j := range w {
			if data[(bit+j)/8]>>uint((bit+j)%8)&1 == 1 {
				v |= 1 << uint(j)
			}
		}
		out[i] = v
		bit += w
	}
	return out
}

// encodeResiduals tries every mode and keeps the smallest, tagged in its first byte
func encodeResiduals(w *zstd.Encoder, r []int64) []byte {
	nonzero := make([]int, 0, len(r))
	var widest, widestAll uint64
	for i, v := range r {
		widestAll = max(widestAll, zigzag(v))
		if v != 0 {
			nonzero = append(nonzero, i)
			widest = max(widest, zigzag(v))
		}
	}
	best := []byte{resVarint}
	for _, v := range r {
		best = binary.AppendVarint(best, v)
	}
	if squeezed := w.EncodeAll(best[1:], nil); len(squeezed)+1 < len(best) {
		best = append([]byte{resVarintZstd}, squeezed...)
	}
	consider := func(candidate []byte) {
		if len(candidate) < len(best) {
			best = candidate
		}
	}
	if len(nonzero) == 0 {
		consider([]byte{resZero})
	}
	bitmap := make([]byte, (len(r)+7)/8)
	for _, i := range nonzero {
		bitmap[i/8] |= 1 << uint(i%8)
	}
	if widest <= 2 { // every nonzero residual is exactly one unit in the last place
		signs := make([]byte, (len(nonzero)+7)/8)
		for k, i := range nonzero {
			if r[i] < 0 {
				signs[k/8] |= 1 << uint(k%8)
			}
		}
		consider(append(append([]byte{resBitmapSign}, bitmap...), signs...))
	}
	if width := bitWidth(widest); width <= 40 && len(nonzero) > 0 {
		vals := make([]uint64, len(nonzero))
		for k, i := range nonzero {
			vals[k] = zigzag(r[i])
		}
		consider(append(append([]byte{resBitmapPacked, byte(width)}, bitmap...), packBits(vals, width)...))
	}
	if width := bitWidth(widestAll); width <= 40 {
		vals := make([]uint64, len(r))
		for i, v := range r {
			vals[i] = zigzag(v)
		}
		consider(append([]byte{resPackedAll, byte(width)}, packBits(vals, width)...))
	}
	sparse := []byte{resSparseGap}
	previous := -1
	for _, i := range nonzero {
		sparse = binary.AppendUvarint(sparse, uint64(i-previous-1))
		sparse = binary.AppendUvarint(sparse, zigzag(r[i]))
		previous = i
	}
	consider(sparse)
	return best
}

func decodeResiduals(t *testing.T, r *zstd.Decoder, data []byte, n int) []int64 {
	t.Helper()
	out := make([]int64, n)
	body := data[1:]
	mode := data[0]
	if mode == resVarintZstd {
		raw, err := r.DecodeAll(body, nil)
		if err != nil {
			t.Fatal(err)
		}
		body, mode = raw, resVarint
	}
	switch mode {
	case resZero:
	case resVarint:
		for i := range out {
			v, k := binary.Varint(body)
			if k <= 0 {
				t.Fatal("residual varint")
			}
			out[i], body = v, body[k:]
		}
	case resBitmapSign:
		bitmap, signs := body[:(n+7)/8], body[(n+7)/8:]
		k := 0
		for i := range out {
			if bitmap[i/8]>>uint(i%8)&1 == 1 {
				out[i] = 1
				if signs[k/8]>>uint(k%8)&1 == 1 {
					out[i] = -1
				}
				k++
			}
		}
	case resBitmapPacked:
		width := int(body[0])
		bitmap := body[1 : 1+(n+7)/8]
		count := 0
		for i := range out {
			if bitmap[i/8]>>uint(i%8)&1 == 1 {
				count++
			}
		}
		vals := unpackBits(body[1+(n+7)/8:], count, width)
		k := 0
		for i := range out {
			if bitmap[i/8]>>uint(i%8)&1 == 1 {
				out[i] = unzigzag(vals[k])
				k++
			}
		}
	case resPackedAll:
		for i, v := range unpackBits(body[1:], n, int(body[0])) {
			out[i] = unzigzag(v)
		}
	case resSparseGap:
		i := -1
		for len(body) > 0 {
			gap, k := binary.Uvarint(body)
			body = body[k:]
			v, k := binary.Uvarint(body)
			body = body[k:]
			i += int(gap) + 1
			out[i] = unzigzag(v)
		}
	default:
		t.Fatal("residual mode")
	}
	return out
}

// quantise puts every sample on the grid 1/den and returns the exact bit corrections
func quantise(samples []codec.Sample, den float64) ([]codec.Sample, []int64, bool) {
	q := make([]codec.Sample, len(samples))
	residual := make([]int64, len(samples)-1)
	for i, s := range samples {
		rounded := math.Round(s.Value * den)
		if math.IsNaN(rounded) || math.IsInf(rounded, 0) || math.Abs(rounded) > 0x1p53 {
			return nil, nil, false
		}
		q[i] = codec.Sample{At: s.At, Value: rounded}
		if i > 0 {
			residual[i-1] = int64(orderedFloat(s.Value) - orderedFloat(rounded/den))
		}
	}
	return q, residual, true
}

type gridStats struct {
	den                     float64
	base, res, total        int
	mode                    byte
	zero, one, small, wider int
}

func TestGridDiagnosis(t *testing.T) {
	series := readCorpus(t)
	c, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	w, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1), zstd.WithWindowSize(8192), zstd.WithLowerEncoderMem(true))
	if err != nil {
		t.Fatal(err)
	}
	defer w.Close()
	r, err := zstd.NewReader(nil, zstd.WithDecoderConcurrency(1), zstd.WithDecoderMaxMemory(8192))
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()

	var decimals, rationals []float64
	for k := range 16 {
		decimals = append(decimals, math.Pow10(k))
	}
	for k := 1; k <= 24; k++ {
		rationals = append(rationals, math.Ldexp(1, k))
	}
	rationals = append(rationals, 3, 6, 7, 9, 12, 24, 30, 60, 300, 360, 3600, 86400, 30000, 60000, 1024000, 3e5, 3e6, 6e4, 6e5)

	const framing = 9 // mode, grid, base length, checksum
	var samples, baseline int
	var decBase, decRes, decTotal, gridTotal int
	var resModeBytes, resModeBlocks [7]int
	histogram := map[int]int{}
	winners := map[float64]int{}
	gridWinners := map[float64]int{}
	var decimalTime, gridTime time.Duration
	var eightExample []float64

	for _, s := range series {
		for start := 0; start < len(s.Values); start += 240 {
			points := corpusSamples(s, start, min(start+240, len(s.Values)))
			_, body, err := c.Encode(points)
			if err != nil {
				t.Fatal(err)
			}
			samples += len(points)
			control := len(body) + 1
			baseline += control

			evaluate := func(dens []float64) (gridStats, bool) {
				best, found := gridStats{}, false
				for _, den := range dens {
					q, residual, ok := quantise(points, den)
					if !ok {
						continue
					}
					_, quantBody, err := c.Encode(q)
					if err != nil {
						t.Fatal(err)
					}
					encoded := encodeResiduals(w, residual)
					total := framing + len(quantBody) + len(encoded)
					if found && total >= best.total {
						continue
					}
					back := decodeResiduals(t, r, encoded, len(residual))
					for i := range residual {
						if back[i] != residual[i] {
							t.Fatal("residual round trip")
						}
					}
					best = gridStats{den: den, base: len(quantBody), res: len(encoded), total: total, mode: encoded[0]}
					for _, v := range residual {
						switch {
						case v == 0:
							best.zero++
						case v == 1 || v == -1:
							best.one++
						case v > -16 && v < 16:
							best.small++
						default:
							best.wider++
						}
					}
					found = true
				}
				return best, found
			}

			begin := time.Now()
			decimal, ok := evaluate(decimals)
			decimalTime += time.Since(begin)
			if ok && decimal.total < control {
				decBase += decimal.base
				decRes += decimal.res
				decTotal += decimal.total
				resModeBytes[decimal.mode] += decimal.res
				resModeBlocks[decimal.mode]++
				winners[decimal.den]++
				histogram[0] += decimal.zero
				histogram[1] += decimal.one
				histogram[2] += decimal.small
				histogram[3] += decimal.wider
				if decimal.den == 1e8 && eightExample == nil {
					eightExample = append([]float64{}, s.Values[start:min(start+6, len(s.Values))]...)
				}
			} else {
				decTotal += control
				decBase += control
			}

			begin = time.Now()
			grid, gok := evaluate(append(append([]float64{}, decimals...), rationals...))
			gridTime += time.Since(begin)
			if gok && grid.total < control {
				gridTotal += grid.total
				gridWinners[grid.den]++
			} else {
				gridTotal += control
			}
		}
	}

	per := func(n int) float64 { return float64(n) / float64(samples) }
	t.Logf("SAMPLES %d", samples)
	t.Logf("CONTROL payload %.4f B/sample", per(baseline))
	t.Logf("DECIMAL total %.4f  base %.4f  residual %.4f  (residual share %.1f%%)",
		per(decTotal), per(decBase), per(decRes), 100*float64(decRes)/float64(decTotal))
	t.Logf("GRID    total %.4f B/sample", per(gridTotal))
	t.Logf("RESIDUAL values: zero=%d pm1=%d small=%d wide=%d", histogram[0], histogram[1], histogram[2], histogram[3])
	for i, name := range []string{"zero", "bitmapSign", "bitmapPacked", "packedAll", "sparseGap", "varint", "varintZstd"} {
		if resModeBlocks[i] > 0 {
			t.Logf("RESIDUAL mode %-13s blocks=%3d bytes=%6d", name, resModeBlocks[i], resModeBytes[i])
		}
	}
	report := func(label string, m map[float64]int) {
		keys := make([]float64, 0, len(m))
		for k := range m {
			keys = append(keys, k)
		}
		sort.Float64s(keys)
		for _, k := range keys {
			t.Logf("%s den=%-12g blocks=%d", label, k, m[k])
		}
	}
	report("DECIMAL-WIN", winners)
	report("GRID-WIN", gridWinners)
	t.Logf("den 1e8 example values %v", eightExample)
	t.Logf("search time decimal=%s grid=%s", decimalTime, gridTime)
}
