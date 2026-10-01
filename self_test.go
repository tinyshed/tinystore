package tinystore

import (
	"context"
	"errors"
	"slices"
	"sync"
	"testing"
	"time"
)

type reportingEngine struct {
	measures []Measure
}

func (e reportingEngine) Close(context.Context) error { return nil }
func (e reportingEngine) Report() []Measure           { return e.measures }

type recordingSelfWriter struct {
	mu      sync.Mutex
	reports [][]Measure
	closed  bool
	written chan struct{}
}

func (w *recordingSelfWriter) WriteSelf(_ context.Context, measures []Measure) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.closed {
		return errors.New("writer was closed before its final report")
	}
	w.reports = append(w.reports, slices.Clone(measures))
	if w.written != nil {
		select {
		case w.written <- struct{}{}:
		default:
		}
	}
	return nil
}

func TestSelfMetricsRegisterBackgroundWorkOnlyWhenEnabled(t *testing.T) {
	store := openTestStore(t, t.TempDir(), Options{SelfMetrics: true})
	writer := &recordingSelfWriter{written: make(chan struct{}, 1)}
	if err := store.Attach(writer); err != nil {
		t.Fatal(err)
	}
	store.self.soon()
	select {
	case <-writer.written:
	case <-time.After(5 * time.Second):
		t.Fatal("enabled store never ran its registered collection")
	}
	if err := store.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
}

func (w *recordingSelfWriter) Close(context.Context) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.closed = true
	return nil
}

func TestSelfMetricsAreOptInAndCollectBeforeClose(t *testing.T) {
	for _, enabled := range []bool{false, true} {
		store := openTestStore(t, t.TempDir(), Options{Manual: true, SelfMetrics: enabled, Memory: 1024})
		writer := &recordingSelfWriter{}
		if err := store.Attach(writer); err != nil {
			t.Fatal(err)
		}
		if err := store.Attach(reportingEngine{[]Measure{{Engine: "sample", Name: "written_total", Value: 7, Counter: true}}}); err != nil {
			t.Fatal(err)
		}
		held, err := store.Reserve(t.Context(), 123)
		if err != nil {
			t.Fatal(err)
		}
		if err = store.FlushSelfMetrics(t.Context()); err != nil {
			t.Fatal(err)
		}
		held.Release()
		if err = store.Close(t.Context()); err != nil {
			t.Fatal(err)
		}
		if !enabled {
			if len(writer.reports) != 0 {
				t.Fatal("disabled store collected reports")
			}
			continue
		}
		if len(writer.reports) != 2 {
			t.Fatalf("reports: %d", len(writer.reports))
		}
		if got := writer.reports[0][0]; got.Name != "memory_used_bytes" || got.Value != 123 {
			t.Fatalf("memory: %+v", got)
		}
		if got := writer.reports[0][4]; got.Value != 7 || !got.Counter {
			t.Fatalf("counter: %+v", got)
		}
		if err = store.FlushSelfMetrics(t.Context()); !errors.Is(err, ErrClosed) {
			t.Fatalf("after close: %v", err)
		}
	}
}

func TestSelfMetricsRefuseAmbiguousOrInexactReports(t *testing.T) {
	for _, measures := range [][]Measure{
		{{Engine: "sample", Name: "bad-name"}},
		{{Engine: "sample", Name: "written", Value: 1<<53 + 1}},
		{{Engine: "sample", Name: "same"}, {Engine: "sample", Name: "same"}},
	} {
		if err := checkSelfMeasures(measures); err == nil {
			t.Fatalf("accepted %+v", measures)
		}
	}
}

func TestSelfMetricsWaitWithTheirContext(t *testing.T) {
	store := openTestStore(t, t.TempDir(), Options{Manual: true, SelfMetrics: true})
	store.self.slot <- struct{}{}
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if err := store.FlushSelfMetrics(ctx); !errors.Is(err, context.Canceled) {
		t.Fatalf("waiting: %v", err)
	}
	<-store.self.slot
}
