package spike

import (
	"fmt"
	"math"
	"math/bits"
	"os"
	"sort"
	"testing"
)

// byHost indexes a corpus the way a measurement is written: one host's fields
// at one instant, which is what any cross-field relation has to be tested on
func byHost(series []corpusSeries) map[string]map[string]corpusSeries {
	out := map[string]map[string]corpusSeries{}
	for _, s := range series {
		host := s.Metric["hostname"]
		if out[host] == nil {
			out[host] = map[string]corpusSeries{}
		}
		out[host][s.Metric["__name__"]] = s
	}
	return out
}

// widthProfile is what a delta stream really costs: the entropy of its widths
// plus the bits under them, which is the bound a Golomb or Elias coder reaches
// and the one a symbol histogram of a near-unique alphabet cannot see
func widthProfile(values []int64) (meanWidth, widthEntropy, maxWidth float64) {
	counts := map[int64]int{}
	var sum float64
	for _, v := range values {
		w := bits.Len64(uint64(v)<<1 ^ uint64(v>>63))
		counts[int64(w)]++
		sum += float64(w)
		maxWidth = math.Max(maxWidth, float64(w))
	}
	if len(values) == 0 {
		return 0, 0, 0
	}
	widthEntropy, _, _ = alphabetEntropy(counts)
	return sum / float64(len(values)), widthEntropy, maxWidth
}

func deltasOf(values []float64) []int64 {
	out := make([]int64, 0, len(values))
	previous, have := int64(0), false
	for _, v := range values {
		n, ok := integerOf(v)
		if !ok {
			have = false
			continue
		}
		if have {
			out = append(out, n-previous)
		}
		previous, have = n, true
	}
	return out
}

// TestMemFieldStructure asks whether the eight fields holding two thirds of the
// payload are eight independent signals or the same signal written eight ways
func TestMemFieldStructure(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	hosts := byHost(readJSONLCorpus(t))
	fields := hosts["host_0"]
	if fields["mem_total"].Values == nil {
		t.Skip("this probe reads the TSBS mem measurement")
	}

	names := make([]string, 0, len(fields))
	for name := range fields {
		if len(name) > 4 && name[:4] == "mem_" {
			names = append(names, name)
		}
	}
	sort.Strings(names)
	for _, name := range names {
		v := fields[name].Values
		t.Logf("%-24s %v", name, v[:4])
	}

	total := fields["mem_total"].Values
	used := fields["mem_used"].Values
	available := fields["mem_available"].Values
	free := fields["mem_free"].Values
	cached := fields["mem_cached"].Values
	buffered := fields["mem_buffered"].Values

	// every identity a memory measurement is supposed to obey, checked on bits
	identities := []struct {
		name string
		sum  func(i int) float64
	}{
		{"used+available", func(i int) float64 { return used[i] + available[i] }},
		{"used+free", func(i int) float64 { return used[i] + free[i] }},
		{"used+free+cached+buffered", func(i int) float64 { return used[i] + free[i] + cached[i] + buffered[i] }},
		{"available+cached+buffered", func(i int) float64 { return available[i] + cached[i] + buffered[i] }},
	}
	for _, id := range identities {
		matches := 0
		for i := range total {
			if math.Float64bits(id.sum(i)) == math.Float64bits(total[i]) {
				matches++
			}
		}
		t.Logf("IDENTITY %-26s == mem_total in %d of %d", id.name, matches, len(total))
	}

	// the three percent fields against every expression a producer might have used
	forms := []struct {
		name string
		of   func(part, whole float64) float64
	}{
		{"part/whole*100", func(p, w float64) float64 { return p / w * 100 }},
		{"part*100/whole", func(p, w float64) float64 { return p * 100 / w }},
		{"100*(part/whole)", func(p, w float64) float64 { return 100 * (p / w) }},
		{"part/(whole/100)", func(p, w float64) float64 { return p / (w / 100) }},
	}
	pairs := []struct{ percent, part string }{
		{"mem_used_percent", "mem_used"},
		{"mem_available_percent", "mem_available"},
		{"mem_buffered_percent", "mem_buffered"},
	}
	for _, pair := range pairs {
		want := fields[pair.percent].Values
		part := fields[pair.part].Values
		for _, form := range forms {
			exact, oneULP, worst := 0, 0, 0
			for i := range want {
				got := form.of(part[i], total[i])
				d := int64(math.Float64bits(got)) - int64(math.Float64bits(want[i]))
				switch d {
				case 0:
					exact++
				case 1, -1:
					oneULP++
				}
				if abs := int(math.Abs(float64(d))); abs > worst && abs < 1<<30 {
					worst = abs
				}
			}
			t.Logf("PERCENT %-22s %-18s exact=%d oneULP=%d of %d worstULP=%d",
				pair.percent, form.name, exact, oneULP, len(want), worst)
		}
	}

	for _, name := range []string{"mem_used", "mem_available", "mem_buffered", "mem_cached", "mem_free", "cpu_usage_user", "diskio_reads"} {
		d := deltasOf(fields[name].Values)
		mean, widthH, maxWidth := widthProfile(d)
		counts := map[int64]int{}
		for _, v := range d {
			counts[v]++
		}
		h0, alphabet, _ := alphabetEntropy(counts)
		lo, hi := d[0], d[0]
		for _, v := range d {
			lo, hi = min(lo, v), max(hi, v)
		}
		t.Logf("DELTA %-16s n=%d range=[%d,%d] alphabet=%d H0=%.2f meanWidth=%.2f H(width)=%.2f maxWidth=%.0f bound=%.2f bits",
			name, len(d), lo, hi, alphabet, h0, mean, widthH, maxWidth, mean+widthH)
	}

	// what the percent series would cost if it were the integer it is a multiple of
	for _, pair := range pairs {
		want := fields[pair.percent].Values
		step := math.Inf(1)
		for i := 1; i < len(want); i++ {
			if d := math.Abs(want[i] - want[i-1]); d > 0 && d < step {
				step = d
			}
		}
		t.Logf("LATTICE %-22s smallest non-zero step=%.20g  100/total=%.20g",
			pair.percent, step, 100/total[0])
	}
	fmt.Fprint(os.Stderr, "")
}

// TestPercentAcrossHosts asks whether a derivation that holds on one host holds
// on all of them, and by how much it misses when it does not
func TestPercentAcrossHosts(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	hosts := byHost(readJSONLCorpus(t))
	if hosts["host_0"]["mem_buffered_percent"].Values == nil {
		t.Skip("this probe reads the TSBS mem measurement")
	}
	for _, host := range sortedHostNames(hosts) {
		fields := hosts[host]
		want := fields["mem_buffered_percent"].Values
		part := fields["mem_buffered"].Values
		total := fields["mem_total"].Values
		exact, oneULP, worst := 0, 0, int64(0)
		distances := map[int64]int{}
		for i := range want {
			got := part[i] / total[i] * 100
			d := int64(math.Float64bits(got)) - int64(math.Float64bits(want[i]))
			distances[d]++
			switch d {
			case 0:
				exact++
			case 1, -1:
				oneULP++
			}
			if d > worst {
				worst = d
			}
			if -d > worst {
				worst = -d
			}
		}
		t.Logf("%-8s mem_buffered_percent exact=%4d oneULP=%4d of %d worst=%d distances=%d",
			host, exact, oneULP, len(want), worst, len(distances))
	}
}
