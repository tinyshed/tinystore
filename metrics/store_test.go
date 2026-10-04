package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

const testEpoch = int64(1000000)

func openTestStore(t *testing.T, options Options) (*Store, string) {
	t.Helper()
	path := filepath.Join(t.TempDir(), fileName)
	store, err := openAt(t, path, options)
	if err != nil {
		t.Fatal(err)
	}
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
	return store, path
}

// openAt opens a Manual store on the directory holding path, a metrics.db, and
// the engine inside it. The test's cleanup closes the store; a test that
// reopens the file closes store.runtime first, which releases the directory.
func openAt(t testing.TB, path string, options Options) (*Store, error) {
	t.Helper()
	if filepath.Base(path) != fileName {
		t.Fatalf("%s: the engine's file is named %s", path, fileName)
	}
	runtime, err := tinystore.Open(t.Context(), filepath.Dir(path), tinystore.Options{Manual: true})
	if err != nil {
		return nil, err
	}
	t.Cleanup(func() {
		if err := runtime.Close(context.Background()); err != nil {
			t.Error(err)
		}
	})
	return Open(t.Context(), runtime, options)
}

func testSeries() Series {
	return Series{Name: "cpu", Labels: Labels{"host": "one"}, Kind: Gauge}
}

// keptOf is a series' labels as the store keeps them, its name among them
func keptOf(t testing.TB, series Series) []label {
	t.Helper()
	kept, err := keptLabels(series.Name, series.Labels)
	if err != nil {
		t.Fatal(err)
	}
	return kept
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
	result, err := store.Read(t.Context(), Range{Name: "cpu", From: math.MinInt64, To: math.MaxInt64})
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
	if err = store.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	store, err = openAt(t, path, options)
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
	other.Name, other.Labels = "other", Labels{}
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
	for _, labels := range []Labels{{"__name__": "cpu"}, {"__anything": "x"}} {
		bad := Series{Name: "cpu", Kind: Gauge, Labels: labels}
		if err = store.Ingest(t.Context(), []Batch{{Series: bad, Samples: points}}); !errors.Is(err, ErrInvalid) {
			t.Fatalf("a label of the store's own, %v: %v", labels, err)
		}
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
		result, err := store.Read(t.Context(), Range{Name: "cpu", From: testEpoch, To: testEpoch + 500, Limits: limits})
		if !errors.Is(err, ErrLimit) || result != nil {
			t.Fatalf("limit %+v: %v, %d results", limits, err, len(result))
		}
	}
	result, err := store.Read(t.Context(), Range{Name: "cpu", Match: Labels{"host": "missing"}, From: testEpoch, To: testEpoch + 500})
	if err != nil || len(result) != 0 {
		t.Fatalf("matcher: %v %#v", err, result)
	}
	result, err = store.Read(t.Context(), Range{Name: testSeries().Name, Match: testSeries().Labels, From: testEpoch + 239, To: testEpoch + 242})
	if err != nil {
		t.Fatal(err)
	}
	assertSamples(t, result[0].Samples, points[239:242])
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err = store.Read(ctx, Range{Name: testSeries().Name, Match: testSeries().Labels, From: 0, To: 1}); !errors.Is(err, context.Canceled) {
		t.Fatalf("cancellation: %v", err)
	}
}

func TestChangedCandidateCannotPublish(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	points := testSamples(500)
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
		t.Fatal(err)
	}
	ids, err := store.readyToSeal(t.Context())
	if err != nil || len(ids) != 1 {
		t.Fatal(ids, err)
	}
	candidate, err := store.readCandidate(t.Context(), ids[0], store.now().UnixMilli())
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
			result, err := store.Read(t.Context(), Range{Name: testSeries().Name, Match: testSeries().Labels, From: testEpoch, To: testEpoch + 800})
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

func TestMetricsErrorsAreTheStoresKinds(t *testing.T) {
	for _, test := range []struct {
		err, kind error
		message   string
	}{
		{ErrInvalid, tinystore.ErrInvalid, "invalid metrics request"},
		{ErrLimit, tinystore.ErrLimit, "metrics resource limit"},
		{ErrClosed, tinystore.ErrClosed, "metrics store is closed"},
		{ErrTooOld, tinystore.ErrTooOld, "sample is expired or sealed"},
		{ErrTooNew, tinystore.ErrTooNew, "sample is ahead of the store's clock"},
		{ErrConflict, tinystore.ErrConflict, "metrics state changed"},
		{ErrCorrupt, tinystore.ErrCorrupt, "corrupt metrics data"},
		{ErrSuspended, tinystore.ErrSuspended, "metrics maintenance is suspended for this series"},
		{ErrNonFinite, tinystore.ErrInvalid, "nonfinite metrics aggregate input"},
		{ErrCounterValue, tinystore.ErrInvalid, "invalid counter aggregate input"},
	} {
		wrapped := fmt.Errorf("read: %w", test.err)
		if !errors.Is(wrapped, test.kind) || !errors.Is(wrapped, test.err) || test.err.Error() != test.message {
			t.Errorf("%q: kind %v", test.err, test.kind)
		}
	}
}

func TestMetricsOpensOncePerStoreAndAFailedOpenLetsGo(t *testing.T) {
	dir := t.TempDir()
	runtime, err := tinystore.Open(t.Context(), dir, tinystore.Options{Manual: true})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = runtime.Close(context.Background()) })

	if err = os.WriteFile(filepath.Join(dir, fileName), []byte("not a database"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err = Open(t.Context(), runtime, Options{}); err == nil {
		t.Fatal("opened a file that is not a database")
	}
	if err = os.Remove(filepath.Join(dir, fileName)); err != nil {
		t.Fatal(err)
	}
	if _, err = Open(t.Context(), runtime, Options{}); err != nil {
		t.Fatalf("open after the failed one gave its name back: %v", err)
	}
	if _, err = Open(t.Context(), runtime, Options{}); !errors.Is(err, tinystore.ErrInUse) {
		t.Fatalf("second metrics in one store: %v", err)
	}
}

func TestClosingTheStoreClosesMetrics(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	if err := store.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: testSamples(1)}})
	if !errors.Is(err, ErrClosed) || !errors.Is(err, tinystore.ErrClosed) {
		t.Fatalf("ingest after the store closed: %v", err)
	}
}

func TestTheStoresClockDecidesWhatHasExpired(t *testing.T) {
	runtime, err := tinystore.Open(t.Context(), t.TempDir(), tinystore.Options{
		Manual: true,
		Clock:  func() time.Time { return time.UnixMilli(testEpoch + 10*time.Hour.Milliseconds()) },
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = runtime.Close(context.Background()) })
	store, err := Open(t.Context(), runtime, Options{Retention: time.Hour})
	if err != nil {
		t.Fatal(err)
	}
	err = store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: testSamples(1)}})
	if !errors.Is(err, ErrTooOld) {
		t.Fatalf("a sample nine hours past retention by the store's clock: %v", err)
	}
}

func TestMaintenanceRunsInTheBackground(t *testing.T) {
	runtime, err := tinystore.Open(t.Context(), t.TempDir(), tinystore.Options{
		Clock: func() time.Time { return time.UnixMilli(testEpoch + 900) },
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = runtime.Close(context.Background()) })
	store, err := Open(t.Context(), runtime, Options{MaintenanceInterval: time.Millisecond})
	if err != nil {
		t.Fatal(err)
	}
	if err = store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: testSamples(241)}}); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(10 * time.Second)
	for store.Stats().SealedBlocks == 0 {
		if time.Now().After(deadline) {
			t.Fatal("no block sealed in ten seconds of background maintenance")
		}
		time.Sleep(time.Millisecond)
	}
}
