package metrics

import (
	"context"
	"errors"
	"math"
	"testing"
)

func TestStreamOwnsResultsAndReportsPartialFailure(t *testing.T) {
	store, _ := openTestStore(t, Options{MaxBatchSamples: 1024})
	first := testSeries()
	second := testSeries()
	second.Labels = []Label{{Name: "host", Value: "two"}, {Name: "__name__", Value: "cpu"}}
	points := testSamples(241)
	if err := store.Ingest(t.Context(), []Batch{{Series: first, Samples: points}, {Series: second, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	request := Range{Matchers: []Label{{Name: "__name__", Value: "cpu"}}, From: testEpoch, To: testEpoch + 241}
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
