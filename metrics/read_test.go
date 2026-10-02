package metrics

import (
	"context"
	"errors"
	"math"
	"testing"
	"time"
)

func TestStreamOwnsResultsAndReportsPartialFailure(t *testing.T) {
	store, _ := openTestStore(t, Options{MaxBatchSamples: 1024})
	first := testSeries()
	second := testSeries()
	second.Name, second.Labels = "cpu", Labels{"host": "two"}
	points := testSamples(241)
	if err := store.Ingest(t.Context(), []Batch{{Series: first, Samples: points}, {Series: second, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	request := Range{Name: "cpu", From: testEpoch, To: testEpoch + 241}
	want, err := store.Read(t.Context(), request)
	if err != nil || len(want) != 2 {
		t.Fatalf("materialized read: %d: %v", len(want), err)
	}
	var got []Result
	if err := store.Stream(t.Context(), request, func(result Result) error {
		got = append(got, result)
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	for series := range want {
		if len(got[series].Samples) != len(want[series].Samples) {
			t.Fatal("stream changed sample count")
		}
		for index, point := range want[series].Samples {
			if got[series].Samples[index].At != point.At || math.Float64bits(got[series].Samples[index].Value) != math.Float64bits(point.Value) {
				t.Fatal("stream changed sample bits")
			}
		}
	}
	stopped := errors.New("consumer stopped")
	called := 0
	if err := store.Stream(t.Context(), request, func(Result) error {
		called++
		return stopped
	}); !errors.Is(err, stopped) || called != 1 {
		t.Fatalf("early stop: calls=%d error=%v", called, err)
	}
	ctx, cancel := context.WithCancel(t.Context())
	called = 0
	if err := store.Stream(ctx, request, func(Result) error {
		called++
		cancel()
		return nil
	}); !errors.Is(err, context.Canceled) || called != 1 {
		t.Fatalf("canceled stream: calls=%d error=%v", called, err)
	}
	if err := store.Stream(t.Context(), request, nil); !errors.Is(err, ErrInvalid) {
		t.Fatalf("nil callback: %v", err)
	}
}

// a range over the last Since starts that long before the store's clock and
// stays open at its end, as a To of zero does; Since and From are one or the
// other
func TestARangeSinceStartsThatLongBeforeNow(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	points := make([]Sample, 900)
	for i := range points {
		points[i] = Sample{At: testEpoch + int64(i), Value: float64(i)}
	}
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
		t.Fatal(err)
	}

	results, err := store.Read(t.Context(), Range{Name: "cpu", Since: 100 * time.Millisecond})
	if err != nil || len(results) != 1 || len(results[0].Samples) != 100 || results[0].Samples[0].At != testEpoch+800 {
		t.Fatalf("the last 100 ms of 900: %+v, %v", results, err)
	}
	buckets, err := store.Aggregate(t.Context(), AggregateRequest{
		Range: Range{Name: "cpu", Since: 100 * time.Millisecond}, Width: 50 * time.Millisecond, Op: AggregateCount,
	})
	if err != nil || len(buckets) != 1 || len(buckets[0].Buckets) != 2 ||
		buckets[0].Buckets[0].From != testEpoch+800 || buckets[0].Buckets[1].Count != 50 {
		t.Fatalf("buckets of the last 100 ms: %+v, %v", buckets, err)
	}
	if results, err = store.Read(t.Context(), Range{Name: "cpu", From: testEpoch + 850}); err != nil ||
		len(results[0].Samples) != 50 {
		t.Fatalf("a range open at its end: %+v, %v", results, err)
	}

	for _, refused := range []Range{
		{Name: "cpu", Since: time.Second, From: testEpoch},
		{Name: "cpu", Since: -time.Second},
		{Name: "cpu", From: testEpoch + 10, To: testEpoch + 5},
	} {
		if _, err = store.Read(t.Context(), refused); !errors.Is(err, ErrInvalid) {
			t.Errorf("%+v: %v", refused, err)
		}
	}
}
