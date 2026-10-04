package metrics

import (
	"database/sql"
	"errors"
	"math"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

func TestOrderedPreparationFallsBackForDuplicatesAndLatePoints(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	series := testSeries()
	first := []Sample{{At: testEpoch + 1, Value: 1}, {At: testEpoch + 3, Value: 3}}
	second := []Sample{{At: testEpoch + 4, Value: 4}, {At: testEpoch + 5, Value: 5}}
	duplicate := math.Float64frombits(0x7ff8000000005678)
	batches := []Batch{
		{Series: series, Samples: first},
		{Series: series, Samples: second},
		{Series: series, Samples: []Sample{{At: testEpoch + 3, Value: duplicate}, {At: testEpoch + 2, Value: 2}}},
	}
	if err := s.Ingest(t.Context(), batches); err != nil {
		t.Fatal(err)
	}
	first[0].Value = 99
	second[0].Value = 99
	want := []Sample{
		{At: testEpoch + 1, Value: 1},
		{At: testEpoch + 2, Value: 2},
		{At: testEpoch + 3, Value: duplicate},
		{At: testEpoch + 4, Value: 4},
		{At: testEpoch + 5, Value: 5},
	}
	assertSamples(t, readAll(t, s), want)
}

// A sample past the store's clock and its skew is refused and names its series,
// so that one wrong clock cannot hold a series' watermark in the future. The
// edge itself is accepted.
func TestASampleAheadOfTheClockIsRefused(t *testing.T) {
	s, _ := openTestStore(t, Options{ClockSkew: time.Minute})
	horizon := testEpoch + 900 + time.Minute.Milliseconds()
	series := testSeries()
	err := s.Ingest(t.Context(), []Batch{{Series: series, Samples: []Sample{{At: horizon + 1, Value: 1}}}})
	var named *SeriesError
	if !errors.As(err, &named) || !errors.Is(err, ErrTooNew) || !errors.Is(err, tinystore.ErrTooNew) {
		t.Fatalf("a sample a millisecond past the skew: %v", err)
	}
	if err = s.Ingest(t.Context(), []Batch{{Series: series, Samples: []Sample{{At: horizon, Value: 1}}}}); err != nil {
		t.Fatalf("a sample at the skew's edge: %v", err)
	}
	assertSamples(t, readAll(t, s), []Sample{{At: horizon, Value: 1}})
}

// The schema admits a histogram series ahead of the engine, so that the kind
// arrives after v0.1.0 without rebuilding the series table; Ingest refuses one.
func TestTheSchemaAdmitsAHistogramTheEngineDoesNotIngestYet(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	if err := store.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(),
			`insert into series(id, identity, label_ids, kind) values (99, '@histogram', x'', 'histogram')`)
		return err
	}); err != nil {
		t.Fatalf("the schema refused a histogram series: %v", err)
	}
	histogram := Batch{Series: Series{Name: "latency", Kind: "histogram"}, Samples: []Sample{{At: testEpoch, Value: 1}}}
	if err := store.Ingest(t.Context(), []Batch{histogram}); !errors.Is(err, ErrInvalid) {
		t.Fatalf("ingested a histogram: %v", err)
	}
}
