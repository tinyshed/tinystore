package metrics

import (
	"context"
	"database/sql"
	"errors"
	"math"
	"path/filepath"
	"sync"
	"testing"
	"time"
)

const testEpoch = int64(1000000)

func openTestStore(t *testing.T, options Options) (*Store, string) {
	t.Helper()
	path := filepath.Join(t.TempDir(), "metrics.db")
	store, err := Open(t.Context(), path, options)
	if err != nil {
		t.Fatal(err)
	}
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
	t.Cleanup(func() {
		if err := store.Close(context.Background()); err != nil {
			t.Error(err)
		}
	})
	return store, path
}

func testSeries() Series {
	return Series{Labels: []Label{{Name: "host", Value: "one"}, {Name: "__name__", Value: "cpu"}}, Kind: Gauge}
}

func testSamples(n int) []Sample {
	out := make([]Sample, n)
	for i := range out {
		value := float64(i%31) / 10
		switch i % 71 {
		case 0:
			value = math.Copysign(0, -1)
		case 1:
			value = math.Float64frombits(0x7ff8000000001234)
		case 2:
			value = math.Inf(1)
		}
		out[i] = Sample{At: testEpoch + int64(i), Value: value}
	}
	return out
}

func readAll(t *testing.T, store *Store) []Sample {
	t.Helper()
	result, err := store.Read(t.Context(), Range{Matchers: []Label{{Name: "__name__", Value: "cpu"}}, From: math.MinInt64, To: math.MaxInt64})
	if err != nil {
		t.Fatal(err)
	}
	if len(result) == 0 {
		return nil
	}
	if len(result) != 1 {
		t.Fatalf("got %d series", len(result))
	}
	return result[0].Samples
}

func assertSamples(t *testing.T, got, want []Sample) {
	t.Helper()
	if len(got) != len(want) {
		t.Fatalf("got %d samples, want %d", len(got), len(want))
	}
	for i, p := range got {
		if p.At != want[i].At || math.Float64bits(p.Value) != math.Float64bits(want[i].Value) {
			t.Fatalf("sample %d changed: %#v / %#v", i, p, want[i])
		}
	}
}

func TestHeadSealingReopenAndPartialRetention(t *testing.T) {
	options := Options{Retention: time.Second, Lateness: 100 * time.Millisecond}
	store, path := openTestStore(t, options)
	points := testSamples(800)
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
		t.Fatal(err)
	}
	assertSamples(t, readAll(t, store), points)
	work, err := store.Maintain(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if work.SealedBlocks != 2 {
		t.Fatalf("sealed %+v", work)
	}
	assertSamples(t, readAll(t, store), points)
	if err = store.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	store, err = Open(t.Context(), path, options)
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close(context.Background())
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
	assertSamples(t, readAll(t, store), points)
	if err = store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: []Sample{{At: testEpoch + 479, Value: 4}}}}); !errors.Is(err, ErrTooOld) {
		t.Fatalf("sealed write: %v", err)
	}
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 1400) }
	assertSamples(t, readAll(t, store), points[400:])
	if _, err = store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	assertSamples(t, readAll(t, store), points[400:])
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 3000) }
	for range 3 {
		if _, err = store.Maintain(t.Context()); err != nil {
			t.Fatal(err)
		}
	}
	if got := readAll(t, store); len(got) != 0 {
		t.Fatal("expired samples remained visible")
	}
	if err = store.file.View(t.Context(), func(tx *sql.Tx) error {
		var bodies, groups int
		readErr := tx.QueryRowContext(t.Context(), `select (select count(*) from payloads),(select count(*) from groups)`).Scan(&bodies, &groups)
		if bodies != 0 || groups != 0 {
			t.Errorf("orphans: payloads=%d groups=%d", bodies, groups)
		}
		return readErr
	}); err != nil {
		t.Fatal(err)
	}
	newPoint := Sample{At: testEpoch + 3000, Value: 42}
	if err = store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: []Sample{newPoint}}}); err != nil {
		t.Fatal(err)
	}
	assertSamples(t, readAll(t, store), []Sample{newPoint})
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 5000) }
	if work, err = store.Maintain(t.Context()); err != nil || work.ExpiredSamples != 1 {
		t.Fatalf("reactivated expiry: %+v %v", work, err)
	}
}

func TestIngestIsAtomicAndLastMutableValueWins(t *testing.T) {
	store, _ := openTestStore(t, Options{MaxSeries: 1, MaxHeadSamples: 4})
	series := testSeries()
	other := testSeries()
	other.Labels = []Label{{Name: "__name__", Value: "other"}}
	points := []Sample{{At: testEpoch + 3, Value: 3}, {At: testEpoch + 1, Value: 1}, {At: testEpoch + 3, Value: 33}}
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	want := []Sample{{At: testEpoch + 1, Value: 1}, {At: testEpoch + 3, Value: 33}}
	assertSamples(t, readAll(t, store), want)
	err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: []Sample{{At: testEpoch + 2, Value: 2}}}, {Series: other, Samples: []Sample{{At: testEpoch + 2, Value: 2}}}})
	if !errors.Is(err, ErrLimit) {
		t.Fatalf("cardinality: %v", err)
	}
	assertSamples(t, readAll(t, store), want)
	err = store.Ingest(t.Context(), []Batch{{Series: series, Samples: []Sample{{At: testEpoch + 2}, {At: testEpoch + 4}, {At: testEpoch + 5}}}})
	if !errors.Is(err, ErrLimit) {
		t.Fatalf("head bound: %v", err)
	}
	assertSamples(t, readAll(t, store), want)
	bad := series
	bad.Labels = []Label{{Name: "__name__", Value: "cpu"}, {Name: "__name__", Value: "cpu"}}
	if err = store.Ingest(t.Context(), []Batch{{Series: bad, Samples: points}}); !errors.Is(err, ErrInvalid) {
		t.Fatalf("duplicate label: %v", err)
	}
}

func TestQueryBudgetsAndMatchers(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	points := testSamples(500)
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	for _, limits := range []Limits{{DecodedSamples: 100}, {PayloadBytes: 1}, {Blocks: 1}, {OutputSamples: 1}} {
		result, err := store.Read(t.Context(), Range{Matchers: []Label{{Name: "__name__", Value: "cpu"}}, From: testEpoch, To: testEpoch + 500, Limits: limits})
		if !errors.Is(err, ErrLimit) || result != nil {
			t.Fatalf("limit %+v: %v, %d results", limits, err, len(result))
		}
	}
	result, err := store.Read(t.Context(), Range{Matchers: []Label{{Name: "host", Value: "missing"}, {Name: "__name__", Value: "cpu"}}, From: testEpoch, To: testEpoch + 500})
	if err != nil || len(result) != 0 {
		t.Fatalf("matcher: %v %#v", err, result)
	}
	result, err = store.Read(t.Context(), Range{Matchers: testSeries().Labels, From: testEpoch + 239, To: testEpoch + 242})
	if err != nil {
		t.Fatal(err)
	}
	assertSamples(t, result[0].Samples, points[239:242])
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err = store.Read(ctx, Range{Matchers: testSeries().Labels, From: 0, To: 1}); !errors.Is(err, context.Canceled) {
		t.Fatalf("cancellation: %v", err)
	}
}

func TestChangedCandidateCannotPublish(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	points := testSamples(500)
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
		t.Fatal(err)
	}
	ids, err := store.dueSeries(t.Context(), true, store.cutoff())
	if err != nil || len(ids) != 1 {
		t.Fatal(ids, err)
	}
	candidate, err := store.readCandidate(t.Context(), ids[0], store.cutoff())
	if err != nil {
		t.Fatal(err)
	}
	group, err := store.encodeCandidate(t.Context(), candidate)
	if err != nil {
		t.Fatal(err)
	}
	points[1].Value = 123
	if err = store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points[1:2]}}); err != nil {
		t.Fatal(err)
	}
	if err = store.publish(t.Context(), candidate, group); !errors.Is(err, ErrConflict) {
		t.Fatalf("stale publish: %v", err)
	}
	assertSamples(t, readAll(t, store), points)
}

func TestCounterSummaryIncludesResets(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	series := testSeries()
	series.Kind = Counter
	points := make([]Sample, 241)
	for i := range points {
		points[i] = Sample{At: testEpoch + int64(i), Value: 20}
	}
	points[0].Value = 100
	points[1].Value = 110
	points[2].Value = 5
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
		group, found, err := store.firstGroup(t.Context(), tx, 1)
		if err != nil {
			return err
		}
		if !found || group.blocks[0].summary.increase != 30 || group.blocks[0].summary.resets != 1 || !group.blocks[0].summary.valid {
			t.Fatalf("counter summary: %#v", group)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	assertSamples(t, readAll(t, store), points)
}

func TestReadersSeeOneSnapshotWhilePackingAndIngesting(t *testing.T) {
	store, _ := openTestStore(t, Options{Retention: time.Second})
	points := testSamples(800)
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 1400) }
	want := points[400:]
	var workers sync.WaitGroup
	failures := make(chan error, 3)
	workers.Go(func() {
		for range 20 {
			if _, err := store.Maintain(t.Context()); err != nil {
				failures <- err
				return
			}
		}
	})
	workers.Go(func() {
		for i := 800; i < 1000; i++ {
			if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: []Sample{{At: testEpoch + int64(i), Value: float64(i)}}}}); err != nil {
				failures <- err
				return
			}
		}
	})
	workers.Go(func() {
		for range 30 {
			result, err := store.Read(t.Context(), Range{Matchers: testSeries().Labels, From: testEpoch, To: testEpoch + 800})
			if err != nil {
				failures <- err
				return
			}
			if len(result) != 1 || len(result[0].Samples) != len(want) {
				failures <- errors.New("snapshot lost samples")
				return
			}
			for i, p := range result[0].Samples {
				if p.At != want[i].At || math.Float64bits(p.Value) != math.Float64bits(want[i].Value) {
					failures <- errors.New("snapshot changed samples")
					return
				}
			}
		}
	})
	workers.Wait()
	close(failures)
	for err := range failures {
		t.Error(err)
	}
}

func TestCloseDrainsAdmittedWorkAndRejectsNewWork(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	if err := store.enter(t.Context()); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if err := store.Close(ctx); !errors.Is(err, context.Canceled) {
		t.Fatalf("close wait: %v", err)
	}
	if err := store.Ingest(t.Context(), nil); !errors.Is(err, ErrClosed) {
		t.Fatalf("closed admission: %v", err)
	}
	store.leave()
	if err := store.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
}
