package spike

import (
	"encoding/binary"
	"math"
	"testing"

	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore/codec"
)

// varintLen is what binary.AppendUvarint would add, without the allocation
func varintLen(v uint64) int {
	n := 1
	for v >= 0x80 {
		v >>= 7
		n++
	}
	return n
}

// patchedFrame packs one group at a fixed width and stores whatever does not
// fit as an index plus a full value, so one wide delta cannot widen its
// neighbours the way a Simple8b selector does
func patchedFrame(group []uint64) int {
	best := math.MaxInt
	for width := range 61 {
		limit := uint64(1)<<uint(width) - 1
		if width == 60 {
			limit = math.MaxUint64
		}
		cost := 2 + (len(group)*width+7)/8
		for _, v := range group {
			if v > limit {
				cost += 1 + varintLen(v)
			}
		}
		best = min(best, cost)
	}
	return best
}

func patched(deltas []uint64, size int) int {
	total := 1
	for start := 0; start < len(deltas); start += size {
		total += patchedFrame(deltas[start:min(start+size, len(deltas))])
	}
	return total
}

// entropyBits is the order-0 floor over the delta alphabet: no arrangement of
// independent symbols beats it, so it says when a packer is finished
func entropyBits(deltas []uint64) float64 {
	counts := map[uint64]int{}
	for _, v := range deltas {
		counts[v]++
	}
	total := float64(len(deltas))
	bits := 0.0
	for _, n := range counts {
		p := float64(n) / total
		bits -= float64(n) * math.Log2(p)
	}
	return bits
}

func zigzagDeltas(values []int64, order int) ([]uint64, bool) {
	current := values
	for range order {
		next := make([]int64, len(current)-1)
		for i := range next {
			next[i] = current[i+1] - current[i]
			if (current[i+1] > current[i]) != (next[i] > 0) && next[i] != 0 {
				return nil, false
			}
		}
		current = next
	}
	out := make([]uint64, len(current))
	for i, v := range current {
		out[i] = zigzag(v)
	}
	return out, true
}

func TestBaseStreamHeadroom(t *testing.T) {
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

	var decimals []float64
	for k := range 16 {
		decimals = append(decimals, math.Pow10(k))
	}

	const framing = 9
	var samples int
	var timeBytes, valueBytes, residualBytes int
	var pfor64, pfor128, pforSecond, varintZstd, huffFloor int
	var entropy float64
	var modelBlocks, fallbackBytes int

	for _, s := range series {
		for start := 0; start < len(s.Values); start += 240 {
			points := corpusSamples(s, start, min(start+240, len(s.Values)))
			_, control, err := c.Encode(points)
			if err != nil {
				t.Fatal(err)
			}
			samples += len(points)

			bestTotal, bestDen, found := len(control)+1, 0.0, false
			var bestQuant []codec.Sample
			var bestResidual []byte
			for _, den := range decimals {
				q, residual, ok := quantise(points, den)
				if !ok {
					continue
				}
				_, quantBody, quantErr := c.Encode(q)
				if quantErr != nil {
					t.Fatal(quantErr)
				}
				encoded := encodeResiduals(w, residual)
				if total := framing + len(quantBody) + len(encoded); total < bestTotal {
					bestTotal, bestDen, found = total, den, true
					bestQuant, bestResidual = q, encoded
				}
			}
			if !found {
				fallbackBytes += len(control) + 1
				continue
			}
			modelBlocks++
			residualBytes += len(bestResidual)

			// a constant-valued block over the same timestamps isolates what the
			// value stream costs from what framing and timestamps cost
			flat := make([]codec.Sample, len(bestQuant))
			for i := range flat {
				flat[i] = codec.Sample{At: bestQuant[i].At, Value: 1}
			}
			_, flatBody, err := c.Encode(flat)
			if err != nil {
				t.Fatal(err)
			}
			_, quantBody, err := c.Encode(bestQuant)
			if err != nil {
				t.Fatal(err)
			}
			timeBytes += len(flatBody) + framing
			valueBytes += len(quantBody) - len(flatBody)

			integers := make([]int64, len(bestQuant))
			for i, q := range bestQuant {
				integers[i] = int64(q.Value)
			}
			first, ok := zigzagDeltas(integers, 1)
			if !ok {
				t.Fatal("first difference overflowed")
			}
			pfor64 += patched(first, 64)
			pfor128 += patched(first, 128)
			if second, ok := zigzagDeltas(integers, 2); ok {
				pforSecond += min(patched(first, 64), patched(second, 64))
			} else {
				pforSecond += patched(first, 64)
			}
			var stream []byte
			for _, v := range first {
				stream = binary.AppendUvarint(stream, v)
			}
			varintZstd += min(len(stream), len(w.EncodeAll(stream, nil))) + 1
			entropy += entropyBits(first)
			huffFloor += int(math.Ceil(entropyBits(first) / 8))
			_ = bestDen
		}
	}

	per := func(n int) float64 { return float64(n) / float64(samples) }
	t.Logf("SAMPLES %d  model blocks %d  fallback bytes %d", samples, modelBlocks, fallbackBytes)
	t.Logf("SPLIT of the model payload, B/sample:")
	t.Logf("  value stream (quantised integers) %.4f", per(valueBytes))
	t.Logf("  timestamps plus framing           %.4f", per(timeBytes))
	t.Logf("  exact residuals                   %.4f", per(residualBytes))
	t.Logf("  total                             %.4f", per(valueBytes+timeBytes+residualBytes+fallbackBytes))
	t.Logf("VALUE STREAM candidates, B/sample:")
	t.Logf("  current codec (simple8b or huffman) %.4f", per(valueBytes))
	t.Logf("  patched frames of 64                %.4f", per(pfor64))
	t.Logf("  patched frames of 128               %.4f", per(pfor128))
	t.Logf("  patched, best of first/second diff   %.4f", per(pforSecond))
	t.Logf("  varint plus zstd                     %.4f", per(varintZstd))
	t.Logf("  order-0 entropy floor of the deltas  %.4f", per(huffFloor))
	t.Logf("RAW entropy %.0f bits over %d samples", entropy, samples)
}
