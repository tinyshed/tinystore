package metrics

import (
	"context"
	"fmt"
	"path/filepath"
	"testing"
	"time"
)

func openBenchStore(b *testing.B) *Store {
	b.Helper()
	store, err := Open(context.Background(), filepath.Join(b.TempDir(), "bench.db"), Options{
		MaxHeadSamples: 1 << 20, MaxHeadBytes: 16 << 20, MaxBatchSamples: 100000, MaxBatchBytes: 64 << 20,
	})
	if err != nil {
		b.Fatal(err)
	}
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
	b.Cleanup(func() { _ = store.Close(context.Background()) })
	return store
}

// one long head that only grows at its end; the append path
func BenchmarkIngestAppendToLongHead(b *testing.B) {
	store := openBenchStore(b)
	series := testSeries()
	if err := store.Ingest(context.Background(), []Batch{{Series: series, Samples: testSamples(2000)}}); err != nil {
		b.Fatal(err)
	}
	next := testEpoch + 2000
	for b.Loop() {
		if err := store.Ingest(context.Background(), []Batch{{Series: series, Samples: []Sample{{At: next, Value: 1}}}}); err != nil {
			b.Fatal(err)
		}
		next++
	}
}

// a late replacement in the middle of a long head
func BenchmarkIngestReplaceInsideHead(b *testing.B) {
	store := openBenchStore(b)
	series := testSeries()
	if err := store.Ingest(context.Background(), []Batch{{Series: series, Samples: testSamples(2000)}}); err != nil {
		b.Fatal(err)
	}
	value := 0.0
	for b.Loop() {
		value++
		if err := store.Ingest(context.Background(), []Batch{{Series: series, Samples: []Sample{{At: testEpoch + 1000, Value: value}}}}); err != nil {
			b.Fatal(err)
		}
	}
}

// a scrape: a hundred series, one new sample each per call
func BenchmarkIngestScrapeOfHundredSeries(b *testing.B) {
	store := openBenchStore(b)
	batches := make([]Batch, 100)
	for i := range batches {
		batches[i].Series = Series{Labels: []Label{{Name: "__name__", Value: "cpu"}, {Name: "core", Value: fmt.Sprint(i)}}}
		batches[i].Samples = testSamples(240)
	}
	if err := store.Ingest(context.Background(), batches); err != nil {
		b.Fatal(err)
	}
	next := testEpoch + 240
	for b.Loop() {
		for i := range batches {
			batches[i].Samples = []Sample{{At: next, Value: float64(i)}}
		}
		if err := store.Ingest(context.Background(), batches); err != nil {
			b.Fatal(err)
		}
		next++
	}
}
