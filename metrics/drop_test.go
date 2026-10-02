package metrics

import (
	"database/sql"
	"errors"
	"fmt"
	"math"
	"testing"
)

func countRows(t *testing.T, s *Store, table string) int {
	t.Helper()
	var count int
	if err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select count(*) from `+table).Scan(&count)
	}); err != nil {
		t.Fatal(err)
	}
	return count
}

func sealedSeries(t *testing.T, s *Store, series ...Series) []Sample {
	t.Helper()
	points := make([]Sample, 481)
	for i := range points {
		points[i] = Sample{At: testEpoch + int64(i), Value: math.Sqrt(float64(i) + 0.5)}
	}
	batches := make([]Batch, len(series))
	for i := range series {
		batches[i] = Batch{Series: series[i], Samples: points}
	}
	if err := s.Ingest(t.Context(), batches); err != nil {
		t.Fatal(err)
	}
	if result, err := s.Maintain(t.Context()); err != nil || result.SealedBlocks == 0 {
		t.Fatalf("seal: %+v, %v", result, err)
	}
	return points
}

func TestDropSeriesRemovesEverythingItHolds(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	series := testSeries()
	sealedSeries(t, s, series)
	if countRows(t, s, "payloads") == 0 {
		t.Fatal("the fixture sealed no external payload")
	}

	dropped, err := s.DropSeries(t.Context(), series.Name, series.Labels)
	if err != nil || !dropped.Found || dropped.UnreadableGroups != 0 {
		t.Fatalf("drop: %+v, %v", dropped, err)
	}
	for _, table := range []string{"series", "series_state", "postings", "label_values", "groups", "payloads", "clocks"} {
		if count := countRows(t, s, table); count != 0 {
			t.Errorf("%s kept %d rows", table, count)
		}
	}
	if count := countRows(t, s, "store_state where series_count = 0"); count != 1 {
		t.Error("the dropped series kept its cardinality slot")
	}

	fresh := []Sample{{At: testEpoch + 10, Value: 7}}
	if err = s.Ingest(t.Context(), []Batch{{Series: series, Samples: fresh}}); err != nil {
		t.Fatal(err)
	}
	results, err := s.Read(t.Context(), Range{Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + 481})
	if err != nil || len(results) != 1 || len(results[0].Samples) != 1 || results[0].Samples[0] != fresh[0] {
		t.Fatalf("after drop: %+v, %v", results, err)
	}
}

func TestDropSeriesKeepsItsNeighbours(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	dropped := Series{Name: "cpu", Labels: Labels{"host": "a"}}
	kept := Series{Name: "cpu", Labels: Labels{"host": "b"}}
	points := sealedSeries(t, s, dropped, kept)

	if result, err := s.DropSeries(t.Context(), dropped.Name, dropped.Labels); err != nil || !result.Found {
		t.Fatalf("drop: %+v, %v", result, err)
	}
	results, err := s.Read(t.Context(), Range{Name: "cpu", From: testEpoch, To: testEpoch + 481})
	if err != nil || len(results) != 1 || len(results[0].Samples) != len(points) {
		t.Fatalf("neighbour: %d results, %v", len(results), err)
	}
	for i, point := range results[0].Samples {
		if point.At != points[i].At || math.Float64bits(point.Value) != math.Float64bits(points[i].Value) {
			t.Fatalf("neighbour sample %d changed", i)
		}
	}
}

func TestDropSeriesRemovesAnUnreadableSuspendedSeries(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	series := testSeries()
	sealedSeries(t, s, series)
	if err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), `update groups set directory = cast(zeroblob(length(directory)) as blob)`)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Read(t.Context(), Range{Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + 481}); !errors.Is(err, ErrCorrupt) {
		t.Fatalf("damaged read: %v", err)
	}
	if _, err := s.handleMaintenanceFailure(t.Context(), 1, "test", fmt.Errorf("%w: test", ErrCorrupt)); err != nil {
		t.Fatal(err)
	}
	payloads := countRows(t, s, "payloads")

	dropped, err := s.DropSeries(t.Context(), series.Name, series.Labels)
	if err != nil || !dropped.Found || dropped.UnreadableGroups != 1 {
		t.Fatalf("drop: %+v, %v", dropped, err)
	}
	if countRows(t, s, "series") != 0 || countRows(t, s, "groups") != 0 || countRows(t, s, "clocks") != 0 {
		t.Fatal("the damaged series left rows behind")
	}
	if countRows(t, s, "payloads") != payloads {
		t.Fatal("payloads no directory names were deleted")
	}
	if stats := s.Stats(); stats.QuarantinedSeries != 0 {
		t.Fatalf("quarantined after drop: %d", stats.QuarantinedSeries)
	}
}

func TestDropSeriesOfAnUnknownSeries(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	if dropped, err := s.DropSeries(t.Context(), testSeries().Name, testSeries().Labels); err != nil || dropped.Found {
		t.Fatalf("unknown series: %+v, %v", dropped, err)
	}
	if _, err := s.DropSeries(t.Context(), "", Labels{"host": "a"}); !errors.Is(err, ErrInvalid) {
		t.Fatalf("labels without a name: %v", err)
	}
}
