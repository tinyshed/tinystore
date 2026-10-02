package metrics

import (
	"context"
	"fmt"
	"path/filepath"
	"testing"
	"time"
)

func BenchmarkWholeBlockAggregate(b *testing.B) {
	for _, operation := range []AggregateOp{AggregateSum, AggregateIncrease} {
		b.Run(string(operation), func(b *testing.B) {
			store, request := aggregateBenchmarkStore(b, operation)
			b.ReportAllocs()
			b.ResetTimer()
			for b.Loop() {
				result, err := store.Aggregate(context.Background(), request)
				if err != nil || len(result) != 8 {
					b.Fatalf("aggregate: %d: %v", len(result), err)
				}
			}
		})
	}
}

func BenchmarkCutBlockAggregate(b *testing.B) {
	store, request := aggregateBenchmarkStore(b, AggregateSum)
	request.Range.From++
	request.Width = 481 * time.Millisecond
	b.ReportAllocs()
	b.ResetTimer()
	for b.Loop() {
		if _, err := store.Aggregate(context.Background(), request); err != nil {
			b.Fatal(err)
		}
	}
}

func aggregateBenchmarkStore(b *testing.B, operation AggregateOp) (*Store, AggregateRequest) {
	b.Helper()
	store, err := openAt(b, filepath.Join(b.TempDir(), fileName), Options{
		MaxHeadSamples: 6000, MaxBatchSamples: 50000, MaxBatchBytes: 16 << 20,
	})
	if err != nil {
		b.Fatal(err)
	}
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 6000) }
	var batches []Batch
	for id := range 8 {
		kind := Gauge
		if operation == AggregateIncrease {
			kind = Counter
		}
		series := Series{Kind: kind, Name: "bench", Labels: Labels{"group": "bench", "id": fmt.Sprint(id)}}
		points := make([]Sample, 4801)
		for i := range points {
			points[i] = Sample{At: testEpoch + int64(i), Value: float64((i*17+id)%997) / 10}
		}
		batches = append(batches, Batch{Series: series, Samples: points})
	}
	if err := store.Ingest(b.Context(), batches); err != nil {
		b.Fatal(err)
	}
	if _, err := store.Maintain(b.Context()); err != nil {
		b.Fatal(err)
	}
	request := AggregateRequest{Range: Range{
		Match: Labels{"group": "bench"},
		From:  testEpoch, To: testEpoch + 4801,
	}, Width: 5 * time.Second, Op: operation}
	return store, request
}
