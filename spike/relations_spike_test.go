package spike

import (
	"hash/fnv"
	"math"
	"os"
	"sort"
	"strings"
	"testing"

	"github.com/tinyshed/tinystore/codec"
)

// instanceKey is every label but the metric's own name: the host, the disk, the
// mount point, the container. Two fields are siblings only when one producer
// wrote them about the same thing, so this and not the host is the grouping.
func instanceKey(metric map[string]string) string {
	keys := make([]string, 0, len(metric))
	for k := range metric {
		if k != "__name__" {
			keys = append(keys, k)
		}
	}
	sort.Strings(keys)
	var b strings.Builder
	for _, k := range keys {
		b.WriteString(k)
		b.WriteByte('=')
		b.WriteString(metric[k])
		b.WriteByte(0)
	}
	return b.String()
}

func groupByInstance(series []corpusSeries) map[string]map[string]corpusSeries {
	out := map[string]map[string]corpusSeries{}
	for _, s := range series {
		key := instanceKey(s.Metric)
		if out[key] == nil {
			out[key] = map[string]corpusSeries{}
		}
		out[key][s.Metric["__name__"]] = s
	}
	return out
}

// clockSignature separates fields a producer wrote at the same instants from
// fields an input with another interval wrote beside them
func clockSignature(times []int64) uint64 {
	h := fnv.New64a()
	var buf [8]byte
	for _, t := range times {
		for i := range buf {
			buf[i] = byte(t >> (8 * i)) //nolint:gosec // a hash of the clock, not its value
		}
		_, _ = h.Write(buf[:])
	}
	return h.Sum64()
}

// a relation whose parameters come from the data needs the data to be varied
// enough to tell it from a coincidence: two flat series satisfy every line
const minDistinct = 16

func distinctValues(values []float64, cap int) int {
	seen := make(map[float64]struct{}, cap)
	for _, v := range values {
		seen[v] = struct{}{}
		if len(seen) >= cap {
			break
		}
	}
	return len(seen)
}

var fittedKinds = map[string]bool{"complement-k": true, "offset-k": true, "scale-k": true, "affine": true, "parts-of-a-whole": true}

// probeSpread is how far a stream moves, which is zero exactly when it is constant
func probeSpread(values []float64, probes []int) float64 {
	lo, hi := values[probes[0]], values[probes[0]]
	for _, i := range probes {
		lo, hi = math.Min(lo, values[i]), math.Max(hi, values[i])
	}
	return hi - lo
}

// partsOfAWhole finds the set of fields that adds up with the target to
// something that does not move. An agent writes the parts of a whole all the
// time — percentages of a CPU, of a disk, of a memory — and no pair of fields
// can see a relation with ten terms in it. The set is grown greedily on a
// sample of the rows; whether it is real is decided afterwards, on every bit of
// every sample, like every other candidate here.
func partsOfAWhole(target []float64, siblings []string, fields map[string]corpusSeries, probes []int) ([]string, float64, bool) {
	running := make([]float64, len(target))
	copy(running, target)
	used := map[string]bool{}
	for range 16 {
		if probeSpread(running, probes) == 0 {
			break
		}
		best, bestSpread := "", probeSpread(running, probes)
		for _, other := range siblings {
			if used[other] {
				continue
			}
			a := fields[other].Values
			lo, hi := math.Inf(1), math.Inf(-1)
			for _, i := range probes {
				v := running[i] + a[i]
				lo, hi = math.Min(lo, v), math.Max(hi, v)
			}
			if hi-lo < bestSpread {
				best, bestSpread = other, hi-lo
			}
		}
		if best == "" {
			return nil, 0, false
		}
		a := fields[best].Values
		for i := range running {
			running[i] += a[i]
		}
		used[best] = true
	}
	if probeSpread(running, probes) != 0 || len(used) == 0 {
		return nil, 0, false
	}
	terms := make([]string, 0, len(used))
	for other := range used {
		terms = append(terms, other)
	}
	sort.Strings(terms)
	return terms, running[probes[0]], true
}

type relation struct {
	kind      string
	from      int // the first index the prediction covers; earlier samples are literal
	residual  []int64
	bits      float64
	reference string
	second    string
	predict   func(i int) float64
}

// searchRelation offers a field every way its siblings could have produced it,
// keeps the cheapest correction stream, and never claims a relation it has not
// checked on every sample.
func searchRelation(fields map[string]corpusSeries, name string, constants map[string]float64, binaryLimit int) *relation {
	target := fields[name]
	values := target.Values
	if len(values) < 2 {
		return nil
	}
	if distinctValues(values, minDistinct) < minDistinct {
		return nil
	}
	signature := clockSignature(target.Times)
	siblings := make([]string, 0, len(fields))
	for other, s := range fields {
		if other == name || len(s.Values) != len(values) || clockSignature(s.Times) != signature {
			continue
		}
		siblings = append(siblings, other)
	}
	sort.Strings(siblings)

	probes := []int{0, len(values) / 3, 2 * len(values) / 3, len(values) - 1}
	var best *relation
	consider := func(kind, reference, second string, from int, predict func(i int) float64) {
		for _, i := range probes {
			if i < from {
				continue
			}
			d := int64(math.Float64bits(predict(i))) - int64(math.Float64bits(values[i])) //nolint:gosec // ULP distance
			if d > maxResidualULP || d < -maxResidualULP {
				return
			}
		}
		residual, ok := ulpResidual(values[from:], func(i int) float64 { return predict(i + from) })
		if !ok {
			return
		}
		counts := map[int64]int{}
		for _, r := range residual {
			counts[r]++
		}
		h, _, total := alphabetEntropy(counts)
		bits := h*float64(total) + float64(from)*64
		if best == nil || bits < best.bits {
			best = &relation{kind: kind, from: from, residual: residual, bits: bits, reference: reference, second: second, predict: predict}
		}
	}

	wholes := make([]string, 0, len(constants))
	for whole := range constants {
		wholes = append(wholes, whole)
	}
	sort.Strings(wholes)

	for _, other := range siblings {
		if _, isConstant := constants[other]; isConstant {
			continue
		}
		a := fields[other].Values
		consider("copy", other, "", 0, func(i int) float64 { return a[i] })
		consider("negated", other, "", 0, func(i int) float64 { return -a[i] })
		// a constant the data names rather than one a sibling holds: two
		// percentages that sum to a hundred, a unit rescale, a fixed offset.
		// every parameter is fitted inside the first quarter and then has to
		// survive the rest, and a reference too flat to fit is refused.
		if distinctValues(a, minDistinct) < minDistinct {
			continue
		}
		sum := values[0] + a[0]
		consider("complement-k", other, "", 0, func(i int) float64 { return sum - a[i] })
		offset := values[0] - a[0]
		consider("offset-k", other, "", 0, func(i int) float64 { return a[i] + offset })
		if a[0] != 0 {
			scale := values[0] / a[0]
			consider("scale-k", other, "", 0, func(i int) float64 { return a[i] * scale })
		}
		for j := 1; j < max(2, len(values)/4); j++ {
			if a[j] == a[0] {
				continue
			}
			slope := (values[j] - values[0]) / (a[j] - a[0])
			intercept := values[0] - slope*a[0]
			consider("affine", other, "", 0, func(i int) float64 { return slope*a[i] + intercept })
			break
		}
		for _, whole := range wholes {
			k := constants[whole]
			consider("complement", other, whole, 0, func(i int) float64 { return k - a[i] })
			consider("ratio-const", other, whole, 0, func(i int) float64 { return a[i] / k * 100 })
			consider("scaled-const", other, whole, 0, func(i int) float64 { return a[i] / k })
		}
	}
	if len(siblings) <= binaryLimit {
		varying := make([]string, 0, len(siblings))
		for _, other := range siblings {
			if _, isConstant := constants[other]; !isConstant {
				varying = append(varying, other)
			}
		}
		wide := make([]int, 0, 64)
		for i := 0; i < len(values) && len(wide) < 64; i += max(1, len(values)/64) {
			wide = append(wide, i)
		}
		if terms, whole, ok := partsOfAWhole(values, varying, fields, wide); ok {
			parts := make([][]float64, len(terms))
			for i, term := range terms {
				parts[i] = fields[term].Values
			}
			consider("parts-of-a-whole", terms[0], strings.Join(terms[1:], "+"), 0, func(i int) float64 {
				rest := 0.0
				for _, part := range parts {
					rest += part[i]
				}
				return whole - rest
			})
		}
	}
	if len(siblings) <= binaryLimit {
		for _, other := range siblings {
			a := fields[other].Values
			for _, third := range siblings {
				if third == other {
					continue
				}
				b := fields[third].Values
				consider("sum", other, third, 0, func(i int) float64 { return a[i] + b[i] })
				consider("difference", other, third, 0, func(i int) float64 { return a[i] - b[i] })
				consider("ratio", other, third, 0, func(i int) float64 { return a[i] / b[i] * 100 })
				// how an agent turns two counters into a percentage: the share of
				// one delta in another over the same interval
				consider("rate-ratio", other, third, 1, func(i int) float64 {
					return (a[i] - a[i-1]) / (b[i] - b[i-1]) * 100
				})
			}
		}
	}
	return best
}

// TestRelationCensus is the instrument both corpora go through: what a field is
// worth today, and what it would be worth as a correction to a sibling
func TestRelationCensus(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	series := readJSONLCorpus(t)
	instances := groupByInstance(series)
	c, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()

	binaryLimit := 96
	current, projected, samples, restored := 0, 0, 0, 0
	kinds, kindBytes, kindWas, exactKinds := map[string]int{}, map[string]int{}, map[string]int{}, map[string]int{}
	widest, skippedBinary := 0, 0
	examples := map[string]string{}

	keys := make([]string, 0, len(instances))
	for key := range instances {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	for _, key := range keys {
		fields := instances[key]
		widest = max(widest, len(fields))
		if len(fields) > binaryLimit {
			skippedBinary++
		}
		constants := map[string]float64{}
		for name, s := range fields {
			if v, ok := constantValue(s.Values); ok {
				constants[name] = v
			}
		}
		found := map[string]*relation{}
		for _, name := range sortedFieldNames(fields) {
			if _, isConstant := constants[name]; isConstant {
				continue
			}
			found[name] = searchRelation(fields, name, constants, binaryLimit)
		}
		// a predicted field may not itself be a prediction, so the graph is one
		// level deep and no reference can be behind another reference
		for _, name := range sortedFieldNames(fields) {
			r := found[name]
			if r == nil {
				continue
			}
			for _, ref := range []string{r.reference, r.second} {
				if ref == "" {
					continue
				}
				if other, predicted := found[ref]; predicted && other != nil {
					if other.reference == name || other.second == name || other.bits < r.bits {
						found[name] = nil
						break
					}
				}
			}
		}
		for _, name := range sortedFieldNames(fields) {
			s := fields[name]
			was := payloadBytes(t, c, s)
			current += was
			samples += len(s.Values)
			r := found[name]
			if _, isConstant := constants[name]; isConstant {
				r = nil
			}
			if r == nil {
				projected += was
				kinds["stored"]++
				kindBytes["stored"] += was
				kindWas["stored"] += was
				continue
			}
			for i := r.from; i < len(s.Values); i++ {
				back := math.Float64frombits(uint64(int64(math.Float64bits(r.predict(i))) - r.residual[i-r.from])) //nolint:gosec // the stored distance
				if math.Float64bits(back) != math.Float64bits(s.Values[i]) {
					t.Fatalf("%s: %s plus its correction changed a sample at %d", name, r.kind, i)
				}
			}
			becomes := packResidual(r.residual) + 8*r.from + 4*(1+len(r.residual)/researchBlock) + 16
			if becomes >= was {
				projected += was
				kinds["stored"]++
				kindBytes["stored"] += was
				kindWas["stored"] += was
				continue
			}
			restored += len(r.residual)
			projected += becomes
			kinds[r.kind]++
			kindBytes[r.kind] += becomes
			kindWas[r.kind] += was
			allZero := true
			for _, d := range r.residual {
				allZero = allZero && d == 0
			}
			if allZero {
				exactKinds[r.kind]++
			}
			if _, seen := examples[r.kind]; !seen {
				examples[r.kind] = name + " <- " + r.reference + " " + r.second
			}
		}
	}

	n := float64(samples)
	t.Logf("RELATIONS over %d series, %d instances, %d samples; widest instance %d fields, %d instances above the binary limit",
		len(series), len(instances), samples, widest, skippedBinary)
	t.Logf("   payload now        %9d B  %.4f B/sample", current, float64(current)/n)
	t.Logf("   payload projected  %9d B  %.4f B/sample  (%.1f%% smaller, %d samples restored)",
		projected, float64(projected)/n, 100*(1-float64(projected)/float64(current)), restored)
	structural, fitted := 0, 0
	for kind, was := range kindWas {
		switch {
		case kind == "stored":
		case fittedKinds[kind]:
			fitted += was
		default:
			structural += was
		}
	}
	t.Logf("   of what a relation replaces, %d B is structural and %d B has a constant fitted from the data (%.2f%% and %.2f%% of the payload)",
		structural, fitted, 100*float64(structural)/float64(current), 100*float64(fitted)/float64(current))
	names := make([]string, 0, len(kinds))
	for kind := range kinds {
		names = append(names, kind)
	}
	sort.Slice(names, func(i, j int) bool { return kindWas[names[i]] > kindWas[names[j]] })
	for _, kind := range names {
		t.Logf("   %-13s series=%5d exact=%5d  was %9d B (%5.2f%%)  becomes %9d B   %s",
			kind, kinds[kind], exactKinds[kind], kindWas[kind],
			100*float64(kindWas[kind])/float64(current), kindBytes[kind], examples[kind])
	}
}
