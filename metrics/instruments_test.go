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
	s.Timer("request_ms").Record(3 * time.Millisecond)
	if err := s.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	reopened, err := openAt(t, path, Options{})
	if err != nil {
		t.Fatal(err)
	}
	reopened.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
	for name, want := range map[string]float64{"requests_total": 7, "request_ms_count": 1, "request_ms_max": 3} {
		if value, _ := lastValue(t, reopened, name, nil); value != want {
			t.Fatalf("%s after close and reopen: %v, want %v", name, value, want)
		}
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

func TestATimerWritesItsCountSumAndLongestAtEachFlush(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	var log bytes.Buffer
	s.log = slog.New(slog.NewTextHandler(&log, nil))
	clock := testEpoch + 900
	s.now = func() time.Time { return time.UnixMilli(clock) }
	latency := s.Timer("request_ms")

	latency.Record(10 * time.Millisecond)
	latency.Record(30 * time.Millisecond)
	latency.Record(2500 * time.Microsecond)
	latency.Record(-time.Millisecond)
	latency.Record(-time.Second)
	flushAt(t, s, &clock, 0)
	flushAt(t, s, &clock, 1000) // nothing measured: the totals again, no longest
	latency.Since(time.Now().Add(-time.Second))
	flushAt(t, s, &clock, 1000)

	for _, want := range []struct {
		name   string
		kind   Kind
		at     []int64
		values []float64
	}{
		{"request_ms_count", Counter, []int64{900, 1900, 2900}, []float64{3, 3, 4}},
		{"request_ms_sum", Counter, []int64{900, 1900, 2900}, []float64{42.5, 42.5, -1}},
		{"request_ms_max", Gauge, []int64{900, 2900}, []float64{30, -1}},
	} {
		samples, kind := samplesOf(t, s, want.name)
		if kind != want.kind || len(samples) != len(want.at) {
			t.Fatalf("%s: %v as %s, want times %v as %s", want.name, samples, kind, want.at, want.kind)
		}
		for i, sample := range samples {
			if sample.At != testEpoch+want.at[i] || (want.values[i] >= 0 && sample.Value != want.values[i]) {
				t.Errorf("%s sample %d: %+v, want %v at %d", want.name, i, sample, want.values[i], want.at[i])
			}
		}
	}
	// the second's sum and longest are whatever Since measured, a second or more
	if sum, _ := lastValue(t, s, "request_ms_sum", nil); sum < 1042.5 {
		t.Errorf("request_ms_sum after Since: %v", sum)
	}
	if longest, _ := lastValue(t, s, "request_ms_max", nil); longest < 1000 {
		t.Errorf("request_ms_max after Since: %v", longest)
	}
	if count := strings.Count(log.String(), "timer duration"); count != 1 {
		t.Errorf("negative durations logged %d times, want once:\n%s", count, log.String())
	}
}

func TestATimersSeriesHaveOneWriter(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	var log bytes.Buffer
	s.log = slog.New(slog.NewTextHandler(&log, nil))

	for range 2 {
		s.Counter("busy_ms_count").Inc()
		s.Timer("busy_ms").Record(time.Millisecond) // its count is the counter's
		s.Timer("idle_ms").Record(time.Millisecond)
		s.Gauge("idle_ms_max").Set(9) // written by the timer
		s.Counter("idle_ms").Inc()    // a series no timer writes
	}
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}

	for _, want := range []struct {
		name  string
		value float64
		kind  Kind
	}{{"busy_ms_count", 2, Counter}, {"idle_ms_count", 2, Counter}, {"idle_ms_max", 1, Gauge}, {"idle_ms", 2, Counter}} {
		if value, kind := lastValue(t, s, want.name, nil); value != want.value || kind != want.kind {
			t.Errorf("%s: %v as %s, want %v as %s", want.name, value, kind, want.value, want.kind)
		}
	}
	if results, err := s.Read(t.Context(), Range{Name: "busy_ms_sum", From: testEpoch, To: testEpoch + 10_000}); err != nil || len(results) != 0 {
		t.Errorf("a refused timer wrote its sum: %+v, %v", results, err)
	}
	for _, line := range []string{"busy_ms_count is a counter already", "written by the timer idle_ms"} {
		if count := strings.Count(log.String(), line); count != 1 {
			t.Errorf("%q logged %d times, want once:\n%s", line, count, log.String())
		}
	}
}

func TestARefusedTimerLeavesItsSeriesOutTogether(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	var log bytes.Buffer
	s.log = slog.New(slog.NewTextHandler(&log, nil))

	s.Counter("good_total").Inc()
	s.Timer("bad_ms").With("", "no name").Record(time.Millisecond)
	s.Timer("bad_ms").With("route").Record(time.Millisecond)
	for range 2 {
		if err := s.Flush(t.Context()); err != nil {
			t.Fatal(err)
		}
	}

	if value, _ := lastValue(t, s, "good_total", nil); value != 1 {
		t.Fatalf("good_total read %v", value)
	}
	// what is left is the timer without labels, which measured nothing
	for _, name := range []string{"bad_ms_count", "bad_ms_sum", "bad_ms_max"} {
		results, err := s.Read(t.Context(), Range{Name: name, From: testEpoch, To: testEpoch + 10_000})
		if err != nil || len(results) > 1 || (len(results) == 1 && len(results[0].Series.Labels) != 0) {
			t.Errorf("%s was written: %+v, %v", name, results, err)
		}
	}
	for _, line := range []string{`series="bad_ms{=`, `label \"route\" has no value`} {
		if count := strings.Count(log.String(), line); count != 1 {
			t.Errorf("%q logged %d times, want once:\n%s", line, count, log.String())
		}
	}
}

func TestAFailedFlushKeepsATimersLongestForTheNext(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	latency := s.Timer("request_ms")
	latency.Record(40 * time.Millisecond)
	cancelled, cancel := context.WithCancel(t.Context())
	cancel()
	if err := s.Flush(cancelled); !errors.Is(err, context.Canceled) {
		t.Fatalf("a flush whose context ended: %v", err)
	}
	latency.Record(7 * time.Millisecond)
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	for name, want := range map[string]float64{"request_ms_count": 2, "request_ms_sum": 47, "request_ms_max": 40} {
		if value, _ := lastValue(t, s, name, nil); value != want {
			t.Errorf("%s: %v, want %v", name, value, want)
		}
	}
}

// flushAt moves the store's clock by step and flushes there
func flushAt(t *testing.T, s *Store, clock *int64, step int64) {
	t.Helper()
	*clock += step
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
}

func samplesOf(t *testing.T, s *Store, name string) ([]Sample, Kind) {
	t.Helper()
	results, err := s.Read(t.Context(), Range{Name: name, From: testEpoch, To: testEpoch + 10_000})
	if err != nil || len(results) != 1 {
		t.Fatalf("read %s: %+v, %v", name, results, err)
	}
	return results[0].Samples, results[0].Series.Kind
}

func TestATimerTheSeriesLimitCutsWritesNoneOfItsSeries(t *testing.T) {
	s, _ := openTestStore(t, Options{MaxSeries: 2})
	s.Counter("good_total").Inc()
	s.Timer("slow_ms").Record(time.Millisecond) // its count fits under the limit, its sum does not
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if value, _ := lastValue(t, s, "good_total", nil); value != 1 {
		t.Fatalf("good_total read %v", value)
	}
	for _, name := range []string{"slow_ms_count", "slow_ms_sum", "slow_ms_max"} {
		if results, err := s.Read(t.Context(), Range{Name: name, From: testEpoch, To: testEpoch + 10_000}); err != nil || len(results) != 0 {
			t.Errorf("%s was written: %+v, %v", name, results, err)
		}
	}
}

func TestATimerLosesNoDurationRecordedWhileFlushing(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	latency := s.Timer("busy_ms")
	var workers sync.WaitGroup
	for range 8 {
		workers.Go(func() {
			for range 1000 {
				latency.With("route", "/a").Record(time.Millisecond)
			}
		})
	}
	for range 10 {
		if err := s.Flush(t.Context()); err != nil {
			t.Fatal(err)
		}
	}
	workers.Wait()
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	for name, want := range map[string]float64{"busy_ms_count": 8000, "busy_ms_sum": 8000} {
		if value, _ := lastValue(t, s, name, Labels{"route": "/a"}); value != want {
			t.Errorf("%s: %v, want %v", name, value, want)
		}
	}
}
