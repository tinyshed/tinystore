package metrics

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

func TestActiveReadAndIngestAdmissionHonorsCancellation(t *testing.T) {
	s, _ := openTestStore(t, Options{MaxConcurrentReads: 1, MaxConcurrentIngest: 1})
	series := testSeries()
	request := Range{Matchers: series.Labels, From: testEpoch, To: testEpoch + 1}
	batch := []Batch{{Series: series, Samples: testSamples(1)}}
	s.readSlots <- struct{}{}
	readCtx, cancelRead := context.WithTimeout(t.Context(), 100*time.Millisecond)
	defer cancelRead()
	if _, err := s.Read(readCtx, request); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("read admission: %v", err)
	}
	<-s.readSlots
	s.ingestSlots <- struct{}{}
	ingestCtx, cancelIngest := context.WithTimeout(t.Context(), 100*time.Millisecond)
	defer cancelIngest()
	if err := s.Ingest(ingestCtx, batch); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("ingest admission: %v", err)
	}
	<-s.ingestSlots
	if err := s.Ingest(t.Context(), batch); err != nil {
		t.Fatalf("ingest after admission: %v", err)
	}
	if _, err := s.Read(t.Context(), request); err != nil {
		t.Fatalf("read after admission: %v", err)
	}
}

func TestStoreMemoryBoundsReadsIngestAndMaintenance(t *testing.T) {
	const capacity = 1 << 30
	runtime, err := tinystore.Open(t.Context(), t.TempDir(), tinystore.Options{Manual: true, Memory: capacity})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = runtime.Close(context.Background()) })
	s, err := Open(t.Context(), runtime, Options{})
	if err != nil {
		t.Fatal(err)
	}
	s.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
	batch := []Batch{{Series: testSeries(), Samples: testSamples(241)}}
	request := Range{Matchers: testSeries().Labels, From: testEpoch, To: testEpoch + 241}
	work := map[string]func(context.Context) error{
		"ingest": func(ctx context.Context) error { return s.Ingest(ctx, batch) },
		"read": func(ctx context.Context) error {
			_, err := s.Read(ctx, request)
			return err
		},
		"maintain": func(ctx context.Context) error {
			_, err := s.Maintain(ctx)
			return err
		},
	}
	for _, name := range []string{"ingest", "read", "maintain"} {
		release, err := runtime.Reserve(t.Context(), capacity)
		if err != nil {
			t.Fatal(err)
		}
		short, cancel := context.WithTimeout(t.Context(), 50*time.Millisecond)
		if err = work[name](short); !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("%s while the store's memory is taken: %v", name, err)
		}
		cancel()
		release()
		if err = work[name](t.Context()); err != nil {
			t.Fatalf("%s once the memory is free: %v", name, err)
		}
		if usage := runtime.Memory(); usage.Used != 0 {
			t.Fatalf("%s kept %d bytes", name, usage.Used)
		}
	}
}
