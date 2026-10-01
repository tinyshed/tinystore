package metrics

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

func TestSelfSamplesDoNotCountThemselvesAndSurviveClose(t *testing.T) {
	dir := t.TempDir()
	now := time.UnixMilli(testEpoch + 900)
	runtime, err := tinystore.Open(t.Context(), dir, tinystore.Options{
		Manual: true, SelfMetrics: true, Clock: func() time.Time { return now },
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = runtime.Close(context.Background()) })
	store, err := Open(t.Context(), runtime, Options{})
	if err != nil {
		t.Fatal(err)
	}
	if err = store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: []Sample{{At: now.UnixMilli(), Value: 42}}}}); err != nil {
		t.Fatal(err)
	}
	for range 3 {
		if err = runtime.FlushSelfMetrics(t.Context()); err != nil {
			t.Fatal(err)
		}
	}
	if got := store.Stats(); got.IngestedSamples != 1 || got.RejectedBatches != 0 {
		t.Fatalf("feedback: %+v", got)
	}
	if err = runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	runtime, err = tinystore.Open(t.Context(), dir, tinystore.Options{Manual: true, Clock: func() time.Time { return now }})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = runtime.Close(context.Background()) })
	store, err = Open(t.Context(), runtime, Options{})
	if err != nil {
		t.Fatal(err)
	}
	got, err := store.Read(t.Context(), Range{From: testEpoch, To: testEpoch + 1000, Matchers: []Label{
		{Name: "__name__", Value: "tinystore_ingested_samples_total"}, {Name: "engine", Value: "metrics"},
	}})
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || len(got[0].Samples) != 1 || got[0].Samples[0].Value != 1 || got[0].Series.Kind != Counter {
		t.Fatalf("self samples: %+v", got)
	}
}

func TestSelfWritesTakeTheClockOnceAndRejectInexactIntegers(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	calls := 0
	store.now = func() time.Time { calls++; return time.UnixMilli(testEpoch + 900) }
	if err := store.WriteSelf(t.Context(), []tinystore.Measure{{Engine: "store", Name: "bytes", Value: 1 << 53}}); err != nil {
		t.Fatal(err)
	}
	if calls != 1 {
		t.Fatalf("clock read %d times", calls)
	}
	if err := store.WriteSelf(t.Context(), []tinystore.Measure{{Engine: "store", Name: "bytes", Value: 1<<53 + 1}}); !errors.Is(err, ErrLimit) {
		t.Fatalf("large integer: %v", err)
	}
	if got := store.Stats(); got.IngestedSamples != 0 || got.RejectedBatches != 0 {
		t.Fatalf("feedback: %+v", got)
	}
}
