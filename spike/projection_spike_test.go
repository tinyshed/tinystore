package spike

import (
	"math"
	"os"
	"sort"
	"testing"

	"github.com/klauspost/compress/huff0"

	"github.com/tinyshed/tinystore/codec"
)

// a prediction against a sibling field, and the distance in units in the last
// place between what it returns and what the producer wrote. The residual is
// what makes the pair exact, so the predictor never has to be the producer's
// own expression.
type prediction struct {
	kind      string
	reference string
	whole     string
	residual  []int64
	bits      float64
	predict   func(i int) float64
}

const maxResidualULP = 8

func ulpResidual(want []float64, predict func(i int) float64) ([]int64, bool) {
	out := make([]int64, len(want))
	for i := range want {
		got := predict(i)
		if math.IsNaN(got) != math.IsNaN(want[i]) {
			return nil, false
		}
		d := int64(math.Float64bits(got)) - int64(math.Float64bits(want[i])) //nolint:gosec // ULP distance
		if d > maxResidualULP || d < -maxResidualULP {
			return nil, false
		}
		out[i] = d
	}
	return out, true
}

func bestPrediction(fields map[string]corpusSeries, name string, constants map[string]float64) *prediction {
	values := fields[name].Values
	var best *prediction
	consider := func(kind, reference, whole string, predict func(i int) float64) {
		residual, ok := ulpResidual(values, predict)
		if !ok {
			return
		}
		counts := map[int64]int{}
		for _, r := range residual {
			counts[r]++
		}
		h, _, total := alphabetEntropy(counts)
		bits := h * float64(total)
		if best == nil || bits < best.bits {
			best = &prediction{kind: kind, reference: reference, whole: whole, residual: residual, bits: bits, predict: predict}
		}
	}
	wholes := make([]string, 0, len(constants))
	for whole := range constants {
		wholes = append(wholes, whole)
	}
	sort.Strings(wholes)
	for _, other := range sortedFieldNames(fields) {
		if other == name || measurementOf(other) != measurementOf(name) {
			continue
		}
		if _, isConstant := constants[other]; isConstant {
			continue
		}
		sibling := fields[other].Values
		consider("duplicate", other, "", func(i int) float64 { return sibling[i] })
		for _, whole := range wholes {
			total := constants[whole]
			if measurementOf(whole) != measurementOf(name) {
				continue
			}
			consider("complement", other, whole, func(i int) float64 { return total - sibling[i] })
			consider("percent", other, whole, func(i int) float64 { return sibling[i] / total * 100 })
		}
	}
	return best
}

// packResidual is what the correction stream costs: a Huffman table of its own
// over a tiny alphabet, or the raw varints when Huffman cannot help
func packResidual(residual []int64) int {
	total := 0
	for start := 0; start < len(residual); start += researchBlock {
		block := residual[start:min(start+researchBlock, len(residual))]
		constant := true
		for _, r := range block {
			constant = constant && r == block[0]
		}
		if constant {
			total += 1 + 8 // a descriptor and the envelope
			continue
		}
		stream := varints(block)
		var scratch huff0.Scratch
		scratch.Reuse = huff0.ReusePolicyNone
		size := len(stream)
		if out, _, err := huff0.Compress1X(stream, &scratch); err == nil && len(out) < size {
			size = len(out)
		}
		total += size + 8 // the same eight bytes of envelope a block pays today
	}
	return total
}

// TestDerivedProjection stacks what the corpus would weigh if a field that is a
// near-exact function of a sibling stored only the distance to it
func TestDerivedProjection(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	series := readJSONLCorpus(t)
	hosts := byHost(series)
	c, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()

	current, projected, samples, restored := 0, 0, 0, 0
	kinds := map[string]int{}
	kindBytes := map[string]int{}
	kindWas := map[string]int{}
	exactKinds := map[string]int{}
	for _, host := range sortedHostNames(hosts) {
		fields := hosts[host]
		constants := map[string]float64{}
		for name, s := range fields {
			if v, ok := constantValue(s.Values); ok {
				constants[name] = v
			}
		}
		// a field may only be predicted from one that is stored, so the
		// dependency is a forest: a predicted field never predicts another
		predicted := map[string]*prediction{}
		for _, name := range sortedFieldNames(fields) {
			if _, isConstant := constants[name]; isConstant {
				continue
			}
			predicted[name] = bestPrediction(fields, name, constants)
		}
		for _, name := range sortedFieldNames(fields) {
			if p := predicted[name]; p != nil && predicted[p.reference] != nil {
				// keep the cheaper of the two and let the other stand on its own
				if other := predicted[p.reference]; other.reference == name || other.bits < p.bits {
					predicted[name] = nil
				}
			}
		}
		for _, name := range sortedFieldNames(fields) {
			s := fields[name]
			was := payloadBytes(t, c, s)
			current += was
			samples += len(s.Values)
			p := predicted[name]
			if p == nil {
				projected += was
				kinds["stored"]++
				kindBytes["stored"] += was
				kindWas["stored"] += was
				continue
			}
			for i, want := range s.Values {
				back := math.Float64frombits(uint64(int64(math.Float64bits(p.predict(i))) - p.residual[i])) //nolint:gosec // the residual is the distance stored
				if math.Float64bits(back) != math.Float64bits(want) {
					t.Fatalf("%s %s: prediction plus residual changed a sample at %d", host, name, i)
				}
			}
			restored += len(s.Values)
			size := packResidual(p.residual)
			projected += size
			kinds[p.kind]++
			kindBytes[p.kind] += size
			kindWas[p.kind] += was
			allZero := true
			for _, r := range p.residual {
				allZero = allZero && r == 0
			}
			if allZero {
				exactKinds[p.kind]++
			}
		}
	}

	n := float64(samples)
	t.Logf("PROJECTION over %d samples, %d of them restored from a sibling and checked on bits", samples, restored)
	t.Logf("   payload now        %9d B  %.4f B/sample", current, float64(current)/n)
	t.Logf("   payload projected  %9d B  %.4f B/sample  (%.1f%% smaller)",
		projected, float64(projected)/n, 100*(1-float64(projected)/float64(current)))
	names := make([]string, 0, len(kinds))
	for kind := range kinds {
		names = append(names, kind)
	}
	sort.Strings(names)
	for _, kind := range names {
		t.Logf("   %-11s series=%4d exact=%4d  was %9d B  becomes %9d B",
			kind, kinds[kind], exactKinds[kind], kindWas[kind], kindBytes[kind])
	}
}
