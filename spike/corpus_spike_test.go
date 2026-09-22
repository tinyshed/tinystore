package spike

import (
	"bufio"
	"encoding/csv"
	"encoding/json"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"testing"
	"time"

	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore/codec"
)

type corpusSeries struct {
	Metric map[string]string `json:"metric"`
	Values []float64         `json:"values"`
	Times  []int64           `json:"timestamps"`
}

func readCorpus(t testing.TB) []corpusSeries {
	t.Helper()
	root := os.Getenv("TINYSTORE_CORPUS")
	if root == "" {
		t.Skip("set TINYSTORE_CORPUS to the pinned NAB subset")
	}
	paths, err := filepath.Glob(filepath.Join(root, "*", "*.csv"))
	if err != nil || len(paths) == 0 {
		t.Fatalf("corpus: %v, %d files", err, len(paths))
	}
	var result []corpusSeries
	for _, path := range paths {
		f, err := os.Open(path)
		if err != nil {
			t.Fatal(err)
		}
		rows, err := csv.NewReader(f).ReadAll()
		f.Close()
		if err != nil {
			t.Fatal(err)
		}
		s := corpusSeries{Metric: map[string]string{"__name__": "nab", "dataset": filepath.Base(filepath.Dir(path)), "series": filepath.Base(path)}}
		for _, row := range rows[1:] {
			at, err := time.Parse("2006-01-02 15:04:05", row[0])
			if err != nil {
				t.Fatal(err)
			}
			v, err := strconv.ParseFloat(row[1], 64)
			if err != nil {
				t.Fatal(err)
			}
			s.Times = append(s.Times, at.UnixMilli())
			s.Values = append(s.Values, v)
		}
		points := corpusSamples(s, 0, len(s.Values))
		sort.SliceStable(points, func(i, j int) bool { return points[i].At < points[j].At })
		s.Times, s.Values = nil, nil
		duplicates := 0
		for _, p := range points {
			last := len(s.Times) - 1
			if last >= 0 && p.At == s.Times[last] {
				duplicates++
				s.Values[last] = p.Value
				continue
			}
			s.Times = append(s.Times, p.At)
			s.Values = append(s.Values, p.Value)
		}
		if duplicates > 0 {
			t.Logf("%s: %d duplicate timestamps, last input value retained for both engines", path, duplicates)
		}
		result = append(result, s)
	}
	return result
}

func corpusSamples(s corpusSeries, start, end int) []codec.Sample {
	out := make([]codec.Sample, end-start)
	for i := range out {
		out[i] = codec.Sample{At: s.Times[start+i], Value: s.Values[start+i]}
	}
	return out
}

func TestRealCorpus(t *testing.T) {
	series := readCorpus(t)
	root := os.Getenv("TINYSTORE_CORPUS")
	f, err := os.Create(filepath.Join(root, "import.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
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
	type totals struct{ samples, base, candidate, blocks, wins int }
	stats := map[string]*totals{}
	for _, s := range series {
		if err := json.NewEncoder(f).Encode(s); err != nil {
			t.Fatal(err)
		}
		group := s.Metric["dataset"]
		if stats[group] == nil {
			stats[group] = new(totals)
		}
		sum := stats[group]
		var baseTotal, bestTotal int
		wins := map[string]int{}
		for start := 0; start < len(s.Values); start += 240 {
			samples := corpusSamples(s, start, min(start+240, len(s.Values)))
			head, body, err := c.Encode(samples)
			if err != nil {
				t.Fatal(err)
			}
			verifyAdaptivePayload(t, c, head, body, samples)
			best := len(body)
			method := "current"
			fixed := len(samples) > 1
			for i := 2; i < len(samples); i++ {
				fixed = fixed && samples[i].At-samples[i-1].At == samples[1].At-samples[0].At
			}
			if fixed {
				for order := 0; order <= 2; order++ {
					raw := reviewTransform(samples, order, true)
					packed := w.EncodeAll(raw, nil)
					back, err := r.DecodeAll(packed, nil)
					if err != nil {
						t.Fatal(err)
					}
					reviewRestore(t, back, samples, order, true)
					if len(packed)+9 < best {
						best = len(packed) + 9
						method = fmt.Sprintf("shuffle%d", order)
					}
					if order > 0 {
						raw = reviewTransform(samples, order, false)
						packed = reviewPack(raw, 239)
						reviewRestore(t, reviewUnpack(t, packed, len(samples)-1, 239), samples, order, false)
						if len(packed)+9 < best {
							best = len(packed) + 9
							method = fmt.Sprintf("pack%d", order)
						}
					}
				}
			}
			baseTotal += len(body)
			bestTotal += best
			wins[method]++
			sum.samples += len(samples)
			sum.base += len(body)
			sum.candidate += best
			sum.blocks++
			if method != "current" {
				sum.wins++
			}
		}
		t.Logf("%s/%s n=%d current=%.4f candidate=%.4f wins=%v", group, s.Metric["series"], len(s.Values), float64(baseTotal)/float64(len(s.Values)), float64(bestTotal)/float64(len(s.Values)), wins)
	}
	for group, s := range stats {
		t.Logf("TOTAL %s samples=%d payload=%.4f candidate=%.4f blocks=%d wins=%d", group, s.samples, float64(s.base)/float64(s.samples), float64(s.candidate)/float64(s.samples), s.blocks, s.wins)
	}
	for _, group := range []string{"realAWSCloudwatch", "realKnownCause", "all"} {
		schema := squeezedSchema("corpus", true, true, false)
		schema.create = append(schema.create, `create table registry(id integer primary key, labels text not null) strict`)
		schema.indexes = append(schema.indexes, `create unique index registry_labels on registry(labels)`)
		db := openNight(t, filepath.Join(t.TempDir(), "corpus.db"), schema)
		tx, err := db.Begin()
		if err != nil {
			t.Fatal(err)
		}
		id, n := 0, 0
		for sid, s := range series {
			if group != "all" && s.Metric["dataset"] != group {
				continue
			}
			labels, err := json.Marshal(s.Metric)
			if err != nil {
				t.Fatal(err)
			}
			if _, err := tx.Exec(`insert into registry values(?,?)`, sid+1, string(labels)); err != nil {
				t.Fatal(err)
			}
			for start := 0; start < len(s.Values); start += 240 {
				samples := corpusSamples(s, start, min(start+240, len(s.Values)))
				_, body, err := c.Encode(samples)
				if err != nil {
					t.Fatal(err)
				}
				old := make([]sample, len(samples))
				for i, s := range samples {
					old[i] = sample{at: s.At, value: s.Value}
				}
				if err := schema.insert(tx, id, int64(sid+1), summarise(old), body); err != nil {
					t.Fatal(err)
				}
				id++
				n += len(samples)
			}
		}
		if err := tx.Commit(); err != nil {
			t.Fatal(err)
		}
		checkpoint(t, db)
		_, bytes := nightShares(t, db, "payloads")
		t.Logf("SQLITE %s samples=%d bytes=%d B/sample=%.4f", group, n, bytes, float64(bytes)/float64(n))
		db.Close()
	}
}

func TestVictoriaCorpusRoundTrip(t *testing.T) {
	path := os.Getenv("TINYSTORE_VM_EXPORT")
	if path == "" {
		t.Skip("set TINYSTORE_VM_EXPORT")
	}
	want := readCorpus(t)
	f, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	got := map[string][]codec.Sample{}
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 4096), 4<<20)
	for scanner.Scan() {
		var s corpusSeries
		if err := json.Unmarshal(scanner.Bytes(), &s); err != nil {
			t.Fatal(err)
		}
		key := s.Metric["dataset"] + "/" + s.Metric["series"]
		got[key] = append(got[key], corpusSamples(s, 0, len(s.Values))...)
	}
	if err := scanner.Err(); err != nil {
		t.Fatal(err)
	}
	total, mismatches := 0, 0
	for _, s := range want {
		key := s.Metric["dataset"] + "/" + s.Metric["series"]
		v := got[key]
		sort.Slice(v, func(i, j int) bool { return v[i].At < v[j].At })
		if len(v) != len(s.Values) {
			t.Fatalf("%s got %d want %d", key, len(v), len(s.Values))
		}
		for i, sample := range v {
			if sample.At != s.Times[i] {
				t.Fatal("timestamp changed")
			}
			if math.Float64bits(sample.Value) != math.Float64bits(s.Values[i]) {
				mismatches++
				if mismatches < 4 {
					t.Logf("changed %s at=%d want=%.17g got=%.17g", key, sample.At, s.Values[i], sample.Value)
				}
			}
		}
		total += len(v)
	}
	t.Logf("VM exported samples=%d bit mismatches=%d", total, mismatches)
}

func BenchmarkRealCorpusEncode(b *testing.B) {
	series := readCorpus(b)
	c, err := codec.New()
	if err != nil {
		b.Fatal(err)
	}
	defer c.Close()
	var blocks [][]codec.Sample
	for _, s := range series {
		for start := 0; start < len(s.Values); start += 240 {
			blocks = append(blocks, corpusSamples(s, start, min(start+240, len(s.Values))))
		}
	}
	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		if _, _, err := c.Encode(blocks[i%len(blocks)]); err != nil {
			b.Fatal(err)
		}
	}
}
