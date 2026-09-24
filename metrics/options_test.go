package metrics

import (
	"database/sql"
	"errors"
	"math"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

// the pool size was a constant, so eight readers queued at two connections
func TestReadersRunAsWideAsTheOptionAllows(t *testing.T) {
	const readers = 6
	store, _ := openTestStore(t, Options{Retention: time.Hour, MaxReaders: readers})
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: testSamples(4)}}); err != nil {
		t.Fatal(err)
	}
	inside, release := make(chan struct{}, readers), make(chan struct{})
	var wait sync.WaitGroup
	for range readers {
		wait.Go(func() {
			_ = store.file.View(t.Context(), func(*sql.Tx) error {
				inside <- struct{}{}
				<-release
				return nil
			})
		})
	}
	deadline := time.After(10 * time.Second)
	for held := range readers {
		select {
		case <-inside:
		case <-deadline:
			close(release)
			wait.Wait()
			t.Fatalf("only %d of %d readers held a snapshot at once", held, readers)
		}
	}
	close(release)
	wait.Wait()
}

func TestAnEmptyReaderPoolIsRefused(t *testing.T) {
	for _, readers := range []int{-1, -8} {
		if _, err := openAt(t, filepath.Join(t.TempDir(), fileName), Options{Retention: time.Hour, MaxReaders: readers}); err == nil {
			t.Fatalf("opened a store with %d readers", readers)
		}
	}
}

func TestZeroOptionsTakeTheDefaults(t *testing.T) {
	got, err := normalizeOptions(Options{})
	if err != nil {
		t.Fatal(err)
	}
	want := Options{
		Retention: 30 * 24 * time.Hour, MaxBlockSpan: 24 * time.Hour, SnapshotTimeout: 5 * time.Second,
		MaintenanceInterval: time.Minute,
		MaxSeries:           100000, MaxHeadSamples: 4096, MaxHeadBytes: 256 << 10, MaxBatchSamples: 10000,
		MaxBatchBytes: 4 << 20, MaintenanceSeries: 64, MaxReaders: 2, MaxConcurrentReads: 2, MaxConcurrentIngest: 1,
		Limits: Limits{Series: 1000, Blocks: 4096, PayloadBytes: 16 << 20, DecodedSamples: 1 << 20, OutputSamples: 100000},
	}
	if got != want {
		t.Fatalf("defaults %+v, want %+v", got, want)
	}
}

func TestEachInvalidOptionIsRefused(t *testing.T) {
	for _, test := range []struct {
		name    string
		options Options
		refusal string
	}{
		{"an unmade shared budget", Options{SharedBudget: &WorkBudget{}}, "uninitialized shared work budget"},
		{"half a millisecond of block span", Options{MaxBlockSpan: 500 * time.Microsecond}, "block span"},
		{"a negative retention", Options{Retention: -time.Hour}, "durations"},
		{"a lateness in microseconds", Options{Lateness: 1500 * time.Microsecond}, "durations"},
		{"a negative snapshot timeout", Options{SnapshotTimeout: -time.Second}, "durations"},
		{"a negative maintenance interval", Options{MaintenanceInterval: -time.Minute}, "durations"},
		{"a negative series capacity", Options{MaxSeries: -1}, "negative capacity"},
		{"more concurrent reads than slots", Options{MaxConcurrentReads: 1<<16 + 1}, "concurrent work capacity"},
		{"a head larger than its format", Options{MaxHeadBytes: maximumHeadBytes + 1}, "mutable head capacity"},
		{"an unbounded query limit", Options{Limits: Limits{Blocks: math.MaxInt}}, "query capacity"},
	} {
		_, err := normalizeOptions(test.options)
		if !errors.Is(err, ErrInvalid) || !strings.HasSuffix(err.Error(), test.refusal) {
			t.Errorf("%s: %v, want %q", test.name, err, test.refusal)
		}
	}
}

func TestQueryLimitsNarrowToTheStore(t *testing.T) {
	store := Limits{Series: 10, Blocks: 20, PayloadBytes: 30, DecodedSamples: 40, OutputSamples: 50}
	got, err := narrowLimits(Limits{Series: 5, PayloadBytes: 31}, store)
	if want := (Limits{Series: 5, Blocks: 20, PayloadBytes: 30, DecodedSamples: 40, OutputSamples: 50}); err != nil || got != want {
		t.Fatalf("narrowed to %+v: %v", got, err)
	}
	if _, err = narrowLimits(Limits{OutputSamples: -1}, store); !errors.Is(err, ErrInvalid) {
		t.Fatalf("a negative query limit: %v", err)
	}
}
