package metrics

import (
	"bytes"
	"context"
	"errors"
	"log/slog"
	"math"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

func TestAConflictingInstrumentCannotChangeTheRegisteredKind(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	var output bytes.Buffer
	s.log = slog.New(slog.NewTextHandler(&output, nil))
	gauge := s.Gauge("shared")
	gauge.Set(5)
	for range 2 {
		s.Counter("shared").Add(3)
	}
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	value, kind := lastValue(t, s, "shared", nil)
	if value != 5 || kind != Gauge || strings.Count(output.String(), "registered as both counter and gauge") != 1 {
		t.Fatalf("conflicting handle changed %s to %v; logs: %s", kind, value, output.String())
	}
}

func TestCounterDropsAnIncrementThatOverflowsItsTotal(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	counter := s.Counter("large_total")
	counter.Add(math.MaxFloat64)
	counter.Add(math.MaxFloat64)
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if value, kind := lastValue(t, s, "large_total", nil); value != math.MaxFloat64 || kind != Counter {
		t.Fatalf("overflow changed counter to %v as %s", value, kind)
	}
}

func TestAnUnpairedInstrumentLabelDoesNotIncrementItsValidPrefix(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	counter := s.Counter("requests_total")
	counter.With("route", "/notes").Inc()
	counter.With("route", "/notes", "method").Inc()
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if value, _ := lastValue(t, s, "", Labels{"route": "/notes"}); value != 1 {
		t.Fatalf("unpaired label incremented a different instrument to %v", value)
	}
}

func TestGaugeFuncCanBeUpdatedWhileFlushing(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	s.GaugeFunc("changing", func(context.Context) (float64, error) { return 1, nil })
	var workers sync.WaitGroup
	workers.Go(func() {
		for range 100 {
			s.GaugeFunc("changing", func(context.Context) (float64, error) { return 2, nil })
		}
	})
	for range 10 {
		if err := s.Flush(t.Context()); err != nil {
			t.Fatal(err)
		}
	}
	workers.Wait()
}

func lastValue(t *testing.T, s *Store, name string, match Labels) (float64, Kind) {
	t.Helper()
	results, err := s.Read(t.Context(), Range{Name: name, Match: match, From: testEpoch, To: testEpoch + 10_000})
	if err != nil || len(results) != 1 {
		t.Fatalf("read %s %v: %+v, %v", name, match, results, err)
	}
	samples := results[0].Samples
	return samples[len(samples)-1].Value, results[0].Series.Kind
}

func TestInstrumentsAreIngestedAtEachFlush(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	requests := s.Counter("requests_total")
	inflight := s.Gauge("inflight")
	s.GaugeFunc("notes", func(context.Context) (float64, error) { return 42, nil })

	requests.Inc()
	requests.Add(2)
	inflight.Set(5)
	inflight.Add(-1)
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}

	for _, want := range []struct {
		name  string
		value float64
		kind  Kind
	}{{"requests_total", 3, Counter}, {"inflight", 4, Gauge}, {"notes", 42, Gauge}} {
		value, kind := lastValue(t, s, want.name, nil)
		if value != want.value || kind != want.kind {
			t.Errorf("%s: %v as %v, want %v as %v", want.name, value, kind, want.value, want.kind)
		}
	}
}

func TestTheSameLabelsInAnyOrderAreOneSeries(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	requests := s.Counter("requests_total")
	requests.With("route", "/notes", "method", "POST").Inc()
	requests.With("method", "POST", "route", "/notes").Inc()
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	value, _ := lastValue(t, s, "", Labels{"route": "/notes"})
	if value != 2 {
		t.Fatalf("two increments of one series read %v", value)
	}
}

func TestARefusedInstrumentDoesNotKeepTheOthersOut(t *testing.T) {
	var log bytes.Buffer
	runtime, err := tinystore.Open(t.Context(), t.TempDir(), tinystore.Options{
		Manual: true, Logger: slog.New(slog.NewTextHandler(&log, nil)),
		Clock: func() time.Time { return time.UnixMilli(testEpoch + 900) },
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = runtime.Close(context.Background()) })
	s, err := Open(t.Context(), runtime, Options{})
	if err != nil {
		t.Fatal(err)
	}

	s.Counter("good_total").Inc()
	s.Counter("bad_total").With("", "no name").Inc()
	s.Counter("negative_total").Add(-1)
	s.GaugeFunc("broken", func(context.Context) (float64, error) { return 0, errors.New("disk gone") })
	for range 2 {
		if err = s.Flush(t.Context()); err != nil {
			t.Fatal(err)
		}
	}

	if value, _ := lastValue(t, s, "good_total", nil); value != 1 {
		t.Fatalf("good_total read %v", value)
	}
	for _, line := range []string{"bad_total", "negative_total", "disk gone"} {
		if count := strings.Count(log.String(), line); count != 1 {
			t.Errorf("%q logged %d times, want once:\n%s", line, count, log.String())
		}
	}
}

func TestClosingTheStoreFlushesTheLastValues(t *testing.T) {
	s, path := openTestStore(t, Options{})
	s.Counter("requests_total").Add(7)
	if err := s.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	reopened, err := openAt(t, path, Options{})
	if err != nil {
		t.Fatal(err)
	}
	reopened.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
	if value, _ := lastValue(t, reopened, "requests_total", nil); value != 7 {
		t.Fatalf("after close and reopen: %v", value)
	}
}

func TestInstrumentsFlushInTheBackground(t *testing.T) {
	runtime, err := tinystore.Open(t.Context(), t.TempDir(), tinystore.Options{
		Clock: func() time.Time { return time.UnixMilli(testEpoch + 900) },
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = runtime.Close(context.Background()) })
	s, err := Open(t.Context(), runtime, Options{Flush: time.Millisecond})
	if err != nil {
		t.Fatal(err)
	}
	s.Gauge("up").Set(1)
	deadline := time.Now().Add(10 * time.Second)
	for s.Stats().IngestedSamples == 0 {
		if time.Now().After(deadline) {
			t.Fatal("nothing flushed in ten seconds")
		}
		time.Sleep(time.Millisecond)
	}
}
