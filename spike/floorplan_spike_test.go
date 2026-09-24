package spike

import (
	"encoding/binary"
	"encoding/json"
	"math"
	"math/bits"
	"os"
	"sort"
	"strings"
	"testing"

	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore/codec"
)

// informationBound is what a field's own deltas cost an ideal coder. A symbol
// histogram is only a bound while symbols repeat: on a near-unique alphabet it
// degenerates to log2(n), so a stream that wide is bounded by its widths instead.
func informationBound(deltas []int64) (bits float64, estimator string) {
	counts := map[int64]int{}
	for _, d := range deltas {
		counts[d]++
	}
	h0, alphabet, total := alphabetEntropy(counts)
	if total == 0 {
		return 0, "empty"
	}
	if alphabet*4 < total {
		return h0 * float64(total), "histogram"
	}
	mean, widthH, _ := widthProfile(deltas)
	return (mean + widthH) * float64(total), "width"
}

func canonicalLabels(metric map[string]string) []byte {
	keys := make([]string, 0, len(metric))
	for k := range metric {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	pairs := make([][2]string, 0, len(keys))
	for _, k := range keys {
		pairs = append(pairs, [2]string{k, metric[k]})
	}
	out, err := json.Marshal(pairs)
	if err != nil {
		panic(err)
	}
	return out
}

// TestCorpusFloor puts the two halves of the file on one page: what the values
// could cost an ideal coder, and what the identity of a series costs beside them
func TestCorpusFloor(t *testing.T) {
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
	writer, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1), zstd.WithWindowSize(1<<20))
	if err != nil {
		t.Fatal(err)
	}
	defer writer.Close()

	// what the values cost now, what they would cost an ideal coder, and what
	// is left once a derivable field is not stored at all
	derivable := map[string]bool{}
	for host, fields := range hosts {
		constants := map[string]float64{}
		for name, s := range fields {
			if v, ok := constantValue(s.Values); ok {
				constants[name] = v
			}
		}
		names := sortedFieldNames(fields)
		for index, name := range names {
			if _, ok := constants[name]; ok {
				continue
			}
			values := fields[name].Values
			for _, other := range names[:index] {
				if _, isConstant := constants[other]; isConstant || measurementOf(other) != measurementOf(name) {
					continue
				}
				sibling := fields[other].Values
				forms := []func(i int) float64{
					func(i int) float64 { return sibling[i] },
				}
				for _, total := range constants {
					forms = append(forms,
						func(i int) float64 { return total - sibling[i] },
						func(i int) float64 { return sibling[i] / total * 100 })
				}
				for _, form := range forms {
					if math.Float64bits(form(0)) != math.Float64bits(values[0]) {
						continue
					}
					if exactFor(values, derivation{check: form}) {
						derivable[host+"\x00"+name] = true
						break
					}
				}
				if derivable[host+"\x00"+name] {
					break
				}
			}
		}
	}

	totalSamples, currentBytes := 0, 0
	floorBits, floorBitsKept := 0.0, 0.0
	estimators := map[string]int{}
	bodies := map[string][]byte{}
	for _, s := range series {
		name := s.Metric["__name__"]
		key := s.Metric["hostname"] + "\x00" + name
		totalSamples += len(s.Values)
		for start := 0; start < len(s.Values); start += researchBlock {
			_, body, encodeErr := c.Encode(corpusSamples(s, start, min(start+researchBlock, len(s.Values))))
			if encodeErr != nil {
				t.Fatal(encodeErr)
			}
			currentBytes += len(body)
			bodies[name] = append(bodies[name], body...)
		}
		deltas := deltasOf(s.Values)
		if len(deltas) == 0 {
			for i := 1; i < len(s.Values); i++ {
				deltas = append(deltas, monotoneInt(s.Values[i])-monotoneInt(s.Values[i-1]))
			}
		}
		bound, estimator := informationBound(deltas)
		estimators[estimator]++
		floorBits += bound
		if !derivable[key] {
			floorBitsKept += bound
		}
	}

	// is there redundancy left between the blocks we already write?
	concatenated, compressed := 0, 0
	for _, body := range bodies {
		concatenated += len(body)
		compressed += len(writer.EncodeAll(body, nil))
	}

	n := float64(totalSamples)
	t.Logf("FLOOR over %d samples", totalSamples)
	t.Logf("   payload now                         %.4f B/sample", float64(currentBytes)/n)
	t.Logf("   an ideal coder over own deltas      %.4f B/sample   (%v)", floorBits/8/n, estimators)
	t.Logf("   the same, derivable fields dropped  %.4f B/sample", floorBitsKept/8/n)
	t.Logf("   every body of a field, zstd'd whole %.4f B/sample   (%d of %d bytes)",
		float64(compressed)/n, compressed, concatenated)

	// the other half of the file: what a series costs before a sample is stored
	labelBytes, pairBytes := 0, 0
	dictionary := map[string]int{}
	for _, s := range series {
		labelBytes += len(canonicalLabels(s.Metric))
		ids := make([]int, 0, len(s.Metric))
		for k, v := range s.Metric {
			pair := k + "\x00" + v
			id, seen := dictionary[pair]
			if !seen {
				id = len(dictionary)
				dictionary[pair] = id
			}
			ids = append(ids, id)
		}
		sort.Ints(ids)
		row := binary.AppendUvarint(nil, uint64(len(ids)))
		previous := 0
		for _, id := range ids {
			row = binary.AppendUvarint(row, uint64(id-previous))
			previous = id
		}
		pairBytes += len(row)
	}
	dictionaryBytes := 0
	for pair := range dictionary {
		dictionaryBytes += len(pair) + 1
	}
	t.Logf("IDENTITY over %d series", len(series))
	t.Logf("   canonical label text                %8d B  %6.1f B/series  %.4f B/sample",
		labelBytes, float64(labelBytes)/float64(len(series)), float64(labelBytes)/n)
	t.Logf("   the same as sorted dictionary ids   %8d B  %6.1f B/series  %.4f B/sample",
		pairBytes, float64(pairBytes)/float64(len(series)), float64(pairBytes)/n)
	t.Logf("   the dictionary behind them          %8d B over %d distinct pairs", dictionaryBytes, len(dictionary))

	// what the hosts disagree about, because a census that averages is not one
	reference := map[string]bool{}
	for name := range hosts["host_0"] {
		reference[name] = derivable["host_0\x00"+name]
	}
	for _, host := range sortedHostNames(hosts) {
		var differs []string
		for name, want := range reference {
			if derivable[host+"\x00"+name] != want {
				differs = append(differs, name)
			}
		}
		if len(differs) > 0 {
			sort.Strings(differs)
			t.Logf("   %s disagrees with host_0 about %s", host, strings.Join(differs, ", "))
		}
	}
}

func sortedHostNames(hosts map[string]map[string]corpusSeries) []string {
	names := make([]string, 0, len(hosts))
	for host := range hosts {
		names = append(names, host)
	}
	sort.Strings(names)
	return names
}

var _ = bits.Len64
