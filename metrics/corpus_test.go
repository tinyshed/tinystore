package metrics

import (
	"bufio"
	"database/sql"
	"encoding/json"
	"math"
	"math/big"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/dbstat"
)

// opt-in integration gate: the public engine, not a prototype schema, owns all corpus writes
func TestCorpusThroughPublicStore(t *testing.T) {
	corpus := os.Getenv("TINYSTORE_JSONL")
	if corpus == "" {
		t.Skip("set TINYSTORE_JSONL to a normalized corpus")
	}
	path := os.Getenv("TINYSTORE_CORPUS_DB")
	if path == "" {
		path = filepath.Join(t.TempDir(), fileName)
	} else if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatal("TINYSTORE_CORPUS_DB must name a new file")
	}
	options := Options{Retention: 100 * 365 * 24 * time.Hour, MaxHeadSamples: 8192, MaxBatchSamples: 8192, Limits: Limits{DecodedSamples: 1000000, OutputSamples: 1000000}}
	store, err := openAt(t, path, options)
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close(t.Context())
	walk := func(visit func(Series, []Sample)) {
		file, openErr := os.Open(corpus)
		if openErr != nil {
			t.Fatal(openErr)
		}
		defer file.Close()
		scanner := bufio.NewScanner(file)
		scanner.Buffer(make([]byte, 1<<20), 1<<28)
		for scanner.Scan() {
			var row struct {
				Metric map[string]string `json:"metric"`
				Values []float64         `json:"values"`
				Times  []int64           `json:"timestamps"`
			}
			if decodeErr := json.Unmarshal(scanner.Bytes(), &row); decodeErr != nil {
				t.Fatal(decodeErr)
			}
			if len(row.Values) != len(row.Times) || len(row.Values) == 0 {
				t.Fatal("corpus lengths")
			}
			series := Series{Kind: Gauge}
			for name, value := range row.Metric {
				series.Labels = append(series.Labels, Label{Name: name, Value: value})
			}
			points := make([]Sample, len(row.Values))
			for i, value := range row.Values {
				points[i] = Sample{At: row.Times[i], Value: value}
			}
			visit(series, points)
		}
		if scanErr := scanner.Err(); scanErr != nil {
			t.Fatal(scanErr)
		}
	}
	begin := time.Now()
	samples, seriesCount := 0, 0
	walk(func(series Series, points []Sample) {
		for start := 0; start < len(points); start += 7681 {
			if writeErr := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points[start:min(start+7681, len(points))]}}); writeErr != nil {
				t.Fatal(writeErr)
			}
			if _, maintainErr := store.Maintain(t.Context()); maintainErr != nil {
				t.Fatal(maintainErr)
			}
		}
		samples += len(points)
		seriesCount++
	})
	if err = store.file.View(t.Context(), func(tx *sql.Tx) error {
		var head, groups, clocks int
		if readErr := tx.QueryRowContext(t.Context(), `select (select coalesce(sum(head_count),0) from series_state),(select count(*) from groups),(select count(*) from clocks)`).Scan(&head, &groups, &clocks); readErr != nil {
			return readErr
		}
		t.Logf("PUBLIC STORE head_samples=%d groups=%d clocks=%d", head, groups, clocks)
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	counters, err := store.file.WriterCounters(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("PUBLIC WRITER commits=%d prepared=%d evicted=%d programs=%d cache_writes=%d cache_spills=%d", counters.Commits, counters.Prepared, counters.Evicted, counters.Programs, counters.CacheWrites, counters.CacheSpills)
	if err = store.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("PUBLIC STORE size is the entire file after close; freed pages are included")
	t.Logf("PUBLIC STORE series=%d samples=%d file=%d B/sample=%.6f ingest+maintain=%s", seriesCount, samples, info.Size(), float64(info.Size())/float64(samples), time.Since(begin))
	objects, err := dbstat.Read(t.Context(), path)
	if err != nil {
		t.Fatal(err)
	}
	for _, object := range objects {
		t.Logf("PUBLIC OBJECT %s bytes=%d", object.Name, object.Bytes)
	}
	reopened, err := openAt(t, path, options)
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close(t.Context())
	begin = time.Now()
	aggregatesChecked := 0
	walk(func(series Series, points []Sample) {
		result, readErr := reopened.Read(t.Context(), Range{Matchers: series.Labels, From: points[0].At, To: points[len(points)-1].At + 1})
		if readErr != nil {
			t.Fatal(readErr)
		}
		if len(result) != 1 {
			t.Fatal("corpus series missing")
		}
		assertSamples(t, result[0].Samples, points)
		if aggregatesChecked < 64 {
			width := time.Duration(points[len(points)-1].At-points[0].At+1) * time.Millisecond
			aggregated, aggregateErr := reopened.Aggregate(t.Context(), AggregateRequest{
				Range: Range{Matchers: series.Labels, From: points[0].At, To: points[len(points)-1].At + 1},
				Width: width, Op: AggregateSum,
			})
			if aggregateErr != nil || len(aggregated) != 1 || len(aggregated[0].Buckets) != 1 {
				t.Fatalf("corpus aggregate: %+v: %v", aggregated, aggregateErr)
			}
			rational := new(big.Rat)
			for _, point := range points {
				rational.Add(rational, new(big.Rat).SetFloat64(point.Value))
			}
			want, _ := rational.Float64()
			bucket := aggregated[0].Buckets[0]
			if bucket.Count != len(points) || math.Float64bits(bucket.Value) != math.Float64bits(want) {
				t.Fatalf("corpus aggregate count=%d value=%v, want count=%d value=%v", bucket.Count, bucket.Value, len(points), want)
			}
			aggregatesChecked++
		}
	})
	t.Logf("PUBLIC STORE full reopened bitwise readback=%s", time.Since(begin))
	t.Logf("PUBLIC STORE exact aggregates checked=%d", aggregatesChecked)
}
