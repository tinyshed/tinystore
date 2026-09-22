package spike

import (
	"fmt"
	"math"
	"os"
	"sort"
	"testing"

	"github.com/tinyshed/tinystore/codec"
)

// a research harness over a real corpus: which fields hold the payload, what
// the values' own entropy is, and what a model shared by a field's instances
// would be allowed to reach. Nothing here is a format.

const researchBlock = codec.MaxSamples

type fieldBudget struct {
	name             string
	series           int
	blocks           int
	samples          int
	bytes            int
	floor            int // the same blocks with one constant value: envelope and timestamps
	modes            [8]int
	integral         int
	deltas           map[int64]int
	blockDeltaBits   float64
	unrepresentable  int
	distinctSampled  map[float64]struct{}
	firstValueSample []float64
}

func (f *fieldBudget) observe(v float64) {
	if len(f.distinctSampled) < 4096 {
		f.distinctSampled[v] = struct{}{}
	}
	if len(f.firstValueSample) < 6 {
		f.firstValueSample = append(f.firstValueSample, v)
	}
}

// integerOf reports the value as the integer it is written as, when the float
// is exactly that integer and nothing else
func integerOf(v float64) (int64, bool) {
	if math.IsNaN(v) || math.IsInf(v, 0) || v < -0x1p62 || v >= 0x1p62 {
		return 0, false
	}
	n := int64(v)
	return n, math.Float64bits(float64(n)) == math.Float64bits(v)
}

func alphabetEntropy(counts map[int64]int) (bits float64, symbols int, total int) {
	for _, n := range counts {
		total += n
	}
	if total == 0 {
		return 0, 0, 0
	}
	for _, n := range counts {
		p := float64(n) / float64(total)
		bits -= p * math.Log2(p)
	}
	return bits, len(counts), total
}

func blockEntropyBits(deltas []int64) float64 {
	counts := map[int64]int{}
	for _, d := range deltas {
		counts[d]++
	}
	bits, _, total := alphabetEntropy(counts)
	return bits * float64(total)
}

// TestValueBudgetByField says which field of which measurement holds the bytes,
// because a per-b-tree division of the file cannot answer that
func TestValueBudgetByField(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	series := readJSONLCorpus(t)
	c, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()

	fields := map[string]*fieldBudget{}
	for _, s := range series {
		name := s.Metric["__name__"]
		f := fields[name]
		if f == nil {
			f = &fieldBudget{name: name, deltas: map[int64]int{}, distinctSampled: map[float64]struct{}{}}
			fields[name] = f
		}
		f.series++
		previous, havePrevious := int64(0), false
		for _, v := range s.Values {
			f.observe(v)
			n, ok := integerOf(v)
			if !ok {
				f.unrepresentable++
				havePrevious = false
				continue
			}
			f.integral++
			if havePrevious {
				f.deltas[n-previous]++
			}
			previous, havePrevious = n, true
		}
		for start := 0; start < len(s.Values); start += researchBlock {
			points := corpusSamples(s, start, min(start+researchBlock, len(s.Values)))
			_, body, encodeErr := c.Encode(points)
			if encodeErr != nil {
				t.Fatal(encodeErr)
			}
			flat := make([]codec.Sample, len(points))
			for i, p := range points {
				flat[i] = codec.Sample{At: p.At, Value: 1}
			}
			_, floor, floorErr := c.Encode(flat)
			if floorErr != nil {
				t.Fatal(floorErr)
			}
			var deltas []int64
			previous, havePrevious := int64(0), false
			for _, p := range points {
				n, ok := integerOf(p.Value)
				if !ok {
					havePrevious = false
					continue
				}
				if havePrevious {
					deltas = append(deltas, n-previous)
				}
				previous, havePrevious = n, true
			}
			f.blockDeltaBits += blockEntropyBits(deltas)
			f.blocks++
			f.samples += len(points)
			f.bytes += len(body)
			f.floor += len(floor)
			f.modes[body[1]>>2&7]++
		}
	}

	ordered := make([]*fieldBudget, 0, len(fields))
	totalBytes, totalSamples, totalFloor := 0, 0, 0
	for _, f := range fields {
		ordered = append(ordered, f)
		totalBytes += f.bytes
		totalSamples += f.samples
		totalFloor += f.floor
	}
	sort.Slice(ordered, func(i, j int) bool { return ordered[i].bytes > ordered[j].bytes })

	t.Logf("CORPUS series=%d samples=%d payload=%d B/sample=%.4f floor=%.4f",
		len(series), totalSamples, totalBytes, float64(totalBytes)/float64(totalSamples),
		float64(totalFloor)/float64(totalSamples))
	t.Logf("%-28s %6s %9s %10s %8s %8s %8s %8s %7s %7s %s",
		"field", "series", "samples", "bytes", "B/smp", "val B/s", "share%", "H0 fam", "H0 blk", "alpha", "modes")
	modeName := []string{"raw", "const", "int", "xor", "scaled"}
	for _, f := range ordered {
		h0, alphabet, _ := alphabetEntropy(f.deltas)
		valueBytes := float64(f.bytes-f.floor) / float64(f.samples)
		modes := ""
		for m, n := range f.modes {
			if n > 0 {
				label := fmt.Sprintf("m%d", m)
				if m < len(modeName) {
					label = modeName[m]
				}
				modes += fmt.Sprintf("%s=%d ", label, n)
			}
		}
		t.Logf("%-28s %6d %9d %10d %8.4f %8.4f %7.2f%% %8.3f %7.3f %7d %s",
			f.name, f.series, f.samples, f.bytes,
			float64(f.bytes)/float64(f.samples), valueBytes,
			100*float64(f.bytes)/float64(totalBytes),
			h0, f.blockDeltaBits/float64(f.samples), alphabet, modes)
	}
	for _, f := range ordered {
		if f.unrepresentable > 0 {
			t.Logf("NON-INTEGER %-24s samples=%d of %d distinct<=%d first=%v",
				f.name, f.unrepresentable, f.samples, len(f.distinctSampled), f.firstValueSample)
		}
	}
}
