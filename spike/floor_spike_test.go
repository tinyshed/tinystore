package spike

import (
	"math"
	"testing"

	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore/codec"
)

// TestDeltaAlphabetIsMostlyDistinct checks whether the order-0 entropy of a
// block's deltas is a floor or an artifact: an alphabet where every symbol
// occurs once hides the whole stream inside the table it does not charge for.
func TestDeltaAlphabetIsMostlyDistinct(t *testing.T) {
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

	var decimals []float64
	for k := range 16 {
		decimals = append(decimals, math.Pow10(k))
	}

	const framing = 9
	var samples, deltaCount, distinctCount, tableBytes int
	var entropy float64
	var widthBits int
	buckets := map[int]int{}

	for _, s := range series {
		for start := 0; start < len(s.Values); start += 240 {
			points := corpusSamples(s, start, min(start+240, len(s.Values)))
			_, control, err := c.Encode(points)
			if err != nil {
				t.Fatal(err)
			}
			samples += len(points)
			bestTotal, found := len(control)+1, false
			var bestQuant []codec.Sample
			for _, den := range decimals {
				q, residual, ok := quantise(points, den)
				if !ok {
					continue
				}
				_, quantBody, err := c.Encode(q)
				if err != nil {
					t.Fatal(err)
				}
				if total := framing + len(quantBody) + len(encodeResiduals(w, residual)); total < bestTotal {
					bestTotal, found, bestQuant = total, true, q
				}
			}
			if !found {
				continue
			}
			integers := make([]int64, len(bestQuant))
			for i, q := range bestQuant {
				integers[i] = int64(q.Value)
			}
			deltas, ok := zigzagDeltas(integers, 1)
			if !ok {
				continue
			}
			counts := map[uint64]int{}
			var widest uint64
			for _, v := range deltas {
				counts[v]++
				widest = max(widest, v)
			}
			deltaCount += len(deltas)
			distinctCount += len(counts)
			entropy += entropyBits(deltas)
			for symbol := range counts {
				tableBytes += varintLen(symbol)
			}
			widthBits += bitWidth(widest) * len(deltas)
			buckets[bitWidth(widest)/8]++
		}
	}

	t.Logf("%d deltas use %d distinct symbols, an alphabet %.1f%% the size of the stream",
		deltaCount, distinctCount, 100*float64(distinctCount)/float64(deltaCount))
	t.Logf("order-0 entropy %.0f B, naive alphabet table %d B, honest sum %.4f B/sample",
		entropy/8, tableBytes, (entropy/8+float64(tableBytes))/float64(samples))
	t.Logf("a fixed width per block would cost %.4f B/sample", float64(widthBits)/8/float64(samples))
	for k, n := range buckets {
		t.Logf("blocks whose widest delta needs %d-%d bits: %d", k*8, k*8+7, n)
	}
}
