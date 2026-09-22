package spike

import (
	"math"
	"os"
	"sort"
	"strings"
	"testing"

	"github.com/tinyshed/tinystore/codec"
)

// measurementOf is the family a field belongs to: everything a producer writes
// in one line of the protocol, at one instant, about one host
func measurementOf(field string) string {
	if cut := strings.IndexByte(field, '_'); cut > 0 {
		return field[:cut]
	}
	return field
}

func payloadBytes(t testing.TB, c *codec.Codec, s corpusSeries) int {
	t.Helper()
	total := 0
	for start := 0; start < len(s.Values); start += researchBlock {
		_, body, err := c.Encode(corpusSamples(s, start, min(start+researchBlock, len(s.Values))))
		if err != nil {
			t.Fatal(err)
		}
		total += len(body)
	}
	return total
}

func constantValue(values []float64) (float64, bool) {
	first := math.Float64bits(values[0])
	for _, v := range values[1:] {
		if math.Float64bits(v) != first {
			return 0, false
		}
	}
	return values[0], true
}

// derivation is one exact way a field can be reconstructed from its siblings,
// checked on bits over every sample rather than on the first few
type derivation struct {
	kind  string
	from  []string
	check func(i int) float64
}

func exactFor(values []float64, d derivation) bool {
	for i := range values {
		if math.Float64bits(d.check(i)) != math.Float64bits(values[i]) {
			return false
		}
	}
	return true
}

// TestDerivableSeriesCensus counts how much of a corpus' payload sits in series
// that carry no information of their own
func TestDerivableSeriesCensus(t *testing.T) {
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

	type verdict struct{ kind, detail string }
	verdicts := map[string]map[string]verdict{}
	bytesOf := map[string]map[string]int{}
	totalBytes, totalSamples := 0, 0
	kindBytes := map[string]int{}
	kindSeries := map[string]int{}

	hostNames := make([]string, 0, len(hosts))
	for host := range hosts {
		hostNames = append(hostNames, host)
	}
	sort.Strings(hostNames)

	for _, host := range hostNames {
		fields := hosts[host]
		verdicts[host] = map[string]verdict{}
		bytesOf[host] = map[string]int{}
		names := make([]string, 0, len(fields))
		for name := range fields {
			names = append(names, name)
		}
		sort.Strings(names)
		for _, name := range names {
			b := payloadBytes(t, c, fields[name])
			bytesOf[host][name] = b
			totalBytes += b
			totalSamples += len(fields[name].Values)
		}
		constants := map[string]float64{}
		for _, name := range names {
			if v, ok := constantValue(fields[name].Values); ok {
				constants[name] = v
			}
		}
		for index, name := range names {
			values := fields[name].Values
			if _, ok := constants[name]; ok {
				verdicts[host][name] = verdict{"constant", ""}
				continue
			}
			found := false
			for _, other := range names[:index] {
				if _, isConstant := constants[other]; isConstant {
					continue
				}
				sibling := fields[other].Values
				if measurementOf(other) != measurementOf(name) {
					continue
				}
				candidates := []derivation{
					{"duplicate", []string{other}, func(i int) float64 { return sibling[i] }},
					{"negated", []string{other}, func(i int) float64 { return -sibling[i] }},
				}
				for whole, total := range constants {
					if measurementOf(whole) != measurementOf(name) {
						continue
					}
					candidates = append(candidates,
						derivation{"complement", []string{other, whole}, func(i int) float64 { return total - sibling[i] }},
						derivation{"percent", []string{other, whole}, func(i int) float64 { return sibling[i] / total * 100 }},
						derivation{"scaled", []string{other, whole}, func(i int) float64 { return sibling[i] / total }},
					)
				}
				for _, candidate := range candidates {
					if math.Float64bits(candidate.check(0)) != math.Float64bits(values[0]) {
						continue
					}
					if !exactFor(values, candidate) {
						continue
					}
					verdicts[host][name] = verdict{candidate.kind, strings.Join(candidate.from, ",")}
					found = true
					break
				}
				if found {
					break
				}
			}
			if !found {
				verdicts[host][name] = verdict{"independent", ""}
			}
		}
		for name, v := range verdicts[host] {
			kindBytes[v.kind] += bytesOf[host][name]
			kindSeries[v.kind]++
		}
	}

	t.Logf("CENSUS series=%d samples=%d payload=%d B/sample=%.4f",
		len(series), totalSamples, totalBytes, float64(totalBytes)/float64(totalSamples))
	kinds := make([]string, 0, len(kindBytes))
	for kind := range kindBytes {
		kinds = append(kinds, kind)
	}
	sort.Slice(kinds, func(i, j int) bool { return kindBytes[kinds[i]] > kindBytes[kinds[j]] })
	for _, kind := range kinds {
		t.Logf("  %-12s series=%4d payload=%9d B  %5.2f%% of payload  %.4f B/sample of corpus",
			kind, kindSeries[kind], kindBytes[kind],
			100*float64(kindBytes[kind])/float64(totalBytes),
			float64(kindBytes[kind])/float64(totalSamples))
	}
	for _, name := range sortedFieldNames(hosts["host_0"]) {
		v := verdicts["host_0"][name]
		if v.kind != "independent" {
			t.Logf("  host_0 %-24s %-10s from %s", name, v.kind, v.detail)
		}
	}
	hostsAgree := 0
	for _, host := range hostNames {
		same := true
		for name, v := range verdicts["host_0"] {
			same = same && verdicts[host][name].kind == v.kind
		}
		if same {
			hostsAgree++
		}
	}
	t.Logf("the same verdicts hold on %d of %d hosts", hostsAgree, len(hostNames))
}

func sortedFieldNames(fields map[string]corpusSeries) []string {
	names := make([]string, 0, len(fields))
	for name := range fields {
		names = append(names, name)
	}
	sort.Strings(names)
	return names
}
