package spike

import (
	"bufio"
	"encoding/json"
	"fmt"
	"math"
	"os"
	"sort"
	"strconv"
	"strings"
	"testing"
)

type point struct {
	at    int64
	value float64
}

func seriesKey(labels map[string]string) string {
	keys := make([]string, 0, len(labels))
	for k := range labels {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	var b strings.Builder
	for _, k := range keys {
		fmt.Fprintf(&b, "%s=%s\x00", k, labels[k])
	}
	return b.String()
}

// readEngineExport reads whatever an engine gave back, in the JSONL shape both
// VictoriaMetrics' export and our own corpus use. One series may arrive in
// several lines, so the points are accumulated and sorted afterwards.
func readEngineExport(t *testing.T, path string) map[string][]point {
	t.Helper()
	f, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 0, 1<<20), 1<<28)
	out := map[string][]point{}
	for scanner.Scan() {
		var s corpusSeries
		if err := json.Unmarshal(scanner.Bytes(), &s); err != nil {
			t.Fatal(err)
		}
		key := seriesKey(s.Metric)
		for i := range s.Values {
			out[key] = append(out[key], point{at: s.Times[i], value: s.Values[i]})
		}
	}
	if err := scanner.Err(); err != nil {
		t.Fatal(err)
	}
	for key := range out {
		sort.Slice(out[key], func(i, j int) bool { return out[key][i].at < out[key][j].at })
	}
	return out
}

// readPromDump reads `promtool tsdb dump`, whose lines are a label set in
// braces, the value and the millisecond timestamp. It prints the shortest form
// that round trips, so the bits survive the text.
func readPromDump(t *testing.T, path string) map[string][]point {
	t.Helper()
	f, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 0, 1<<20), 1<<24)
	out := map[string][]point{}
	for scanner.Scan() {
		line := scanner.Text()
		close := strings.LastIndex(line, "}")
		if !strings.HasPrefix(line, "{") || close < 0 {
			t.Fatalf("unexpected dump line: %.80s", line)
		}
		labels := map[string]string{}
		for _, pair := range splitDumpLabels(line[1:close]) {
			key, quoted, ok := strings.Cut(pair, "=")
			if !ok {
				t.Fatalf("unexpected label: %s", pair)
			}
			if strings.HasPrefix(key, "\"") {
				key, err = strconv.Unquote(key)
				if err != nil {
					t.Fatalf("unquote dump label name %q: %v", key, err)
				}
			}
			value, err := strconv.Unquote(quoted)
			if err != nil {
				t.Fatalf("unquote dump label %q in %.200s: %v", quoted, line, err)
			}
			labels[key] = value
		}
		rest := strings.Fields(line[close+1:])
		if len(rest) != 2 {
			t.Fatalf("unexpected sample: %s", line[close+1:])
		}
		value, err := strconv.ParseFloat(rest[0], 64)
		if err != nil {
			t.Fatal(err)
		}
		at, err := strconv.ParseInt(rest[1], 10, 64)
		if err != nil {
			t.Fatal(err)
		}
		key := seriesKey(labels)
		out[key] = append(out[key], point{at: at, value: value})
	}
	if err := scanner.Err(); err != nil {
		t.Fatal(err)
	}
	for key := range out {
		sort.Slice(out[key], func(i, j int) bool { return out[key][i].at < out[key][j].at })
	}
	return out
}

func splitDumpLabels(input string) []string {
	var parts []string
	start := 0
	quoted, escaped := false, false
	for i := 0; i < len(input); i++ {
		switch {
		case escaped:
			escaped = false
		case quoted && input[i] == '\\':
			escaped = true
		case input[i] == '"':
			quoted = !quoted
		case !quoted && input[i] == ',' && i+1 < len(input) && input[i+1] == ' ':
			parts = append(parts, input[start:i])
			i++
			start = i + 1
		}
	}
	return append(parts, input[start:])
}

// TestEngineExportIsBitExact reports, rather than asserts, how an engine's
// round trip compares with the corpus, because a passing assertion would claim
// a certification this path cannot give.
func TestEngineExportIsBitExact(t *testing.T) {
	export, dump := os.Getenv("TINYSTORE_ENGINE_EXPORT"), os.Getenv("TINYSTORE_PROM_DUMP")
	if export == "" && dump == "" {
		t.Skip("set TINYSTORE_ENGINE_EXPORT or TINYSTORE_PROM_DUMP to an engine's output")
	}
	// a whole dump of a large corpus is tens of gigabytes of text, so a run may
	// check a stated window instead; the window is part of the result
	from, to := int64(math.MinInt64), int64(math.MaxInt64)
	if window := os.Getenv("TINYSTORE_WINDOW"); window != "" {
		low, high, ok := strings.Cut(window, "-")
		if !ok {
			t.Fatalf("TINYSTORE_WINDOW wants fromMillis-toMillis, got %q", window)
		}
		var err error
		if from, err = strconv.ParseInt(low, 10, 64); err != nil {
			t.Fatal(err)
		}
		if to, err = strconv.ParseInt(high, 10, 64); err != nil {
			t.Fatal(err)
		}
	}

	corpus := readJSONLCorpus(t)
	var got map[string][]point
	if dump != "" {
		got = readPromDump(t, dump)
	} else {
		got = readEngineExport(t, export)
	}

	var expected, matched, missingSeries, missingSamples, extraSamples, wrongBits int
	var firstWrong string
	for _, s := range corpus {
		key := seriesKey(s.Metric)
		inWindow := 0
		for _, at := range s.Times {
			if at >= from && at <= to {
				inWindow++
			}
		}
		expected += inWindow
		points, ok := got[key]
		if !ok {
			if inWindow > 0 {
				missingSeries++
				missingSamples += inWindow
			}
			continue
		}
		byTime := make(map[int64]float64, len(points))
		for _, p := range points {
			byTime[p.at] = p.value
		}
		for i, at := range s.Times {
			if at < from || at > to {
				continue
			}
			value, ok := byTime[at]
			if !ok {
				missingSamples++
				continue
			}
			matched++
			if math.Float64bits(value) != math.Float64bits(s.Values[i]) {
				wrongBits++
				if firstWrong == "" {
					firstWrong = fmt.Sprintf("%s at %d: %v became %v", s.Metric["__name__"], at, s.Values[i], value)
				}
			}
		}
		extraSamples += len(points) - inWindow
		delete(got, key)
	}
	for key := range got {
		extraSamples += len(got[key])
		_ = key
	}
	if from != math.MinInt64 {
		t.Logf("window %d..%d milliseconds", from, to)
	}
	t.Logf("corpus samples in scope %d, series %d", expected, len(corpus))
	t.Logf("matched %d, missing series %d, missing samples %d, extra samples %d",
		matched, missingSeries, missingSamples, extraSamples)
	t.Logf("values that did not return bit for bit: %d", wrongBits)
	if firstWrong != "" {
		t.Logf("first difference: %s", firstWrong)
	}
}
