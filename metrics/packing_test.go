package metrics

import (
	"database/sql"
	"testing"
	"time"
)

func TestMaintenanceRotatesReadySeries(t *testing.T) {
	s, _ := openTestStore(t, Options{MaintenanceSeries: 1})
	a := Series{Labels: []Label{{Name: "__name__", Value: "a"}}}
	b := Series{Labels: []Label{{Name: "__name__", Value: "b"}}}
	points := testSamples(481)
	if err := s.Ingest(t.Context(), []Batch{{Series: a, Samples: points[:241]}, {Series: b, Samples: points[:241]}}); err != nil {
		t.Fatal(err)
	}
	first, err := s.Maintain(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if first.SealedBlocks != 1 {
		t.Fatalf("first pass sealed %d blocks", first.SealedBlocks)
	}
	if err = s.Ingest(t.Context(), []Batch{{Series: a, Samples: points[241:]}}); err != nil {
		t.Fatal(err)
	}
	second, err := s.Maintain(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if second.SealedBlocks != 1 {
		t.Fatalf("second pass sealed %d blocks", second.SealedBlocks)
	}
	if err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		var frontier sql.NullInt64
		if err := tx.QueryRowContext(t.Context(), `select state.sealed_before from series_state state join postings p on p.series_id=state.series_id join label_values v on v.id=p.label_id where v.name='__name__' and v.value='b'`).Scan(&frontier); err != nil {
			return err
		}
		if !frontier.Valid || frontier.Int64 != points[240].At {
			t.Fatalf("later series frontier: %v", frontier)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func TestReadyWaitsForASealableWatermarkPrefix(t *testing.T) {
	s, _ := openTestStore(t, Options{Lateness: time.Hour, MaintenanceSeries: 1})
	series := testSeries()
	points := make([]Sample, 240)
	for i := range points {
		points[i] = Sample{At: testEpoch + int64(i)*10000, Value: float64(i)}
	}
	if err := s.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	checkReady := func(want int) {
		t.Helper()
		if err := s.file.View(t.Context(), func(tx *sql.Tx) error {
			var ready int
			if err := tx.QueryRowContext(t.Context(), `select ready from series_state where series_id=1`).Scan(&ready); err != nil {
				return err
			}
			if ready != want {
				t.Fatalf("ready=%d, want %d", ready, want)
			}
			return nil
		}); err != nil {
			t.Fatal(err)
		}
	}
	checkReady(0)
	for i := 240; i < 260; i++ {
		point := Sample{At: testEpoch + int64(i)*10000, Value: float64(i)}
		if err := s.Ingest(t.Context(), []Batch{{Series: series, Samples: []Sample{point}}}); err != nil {
			t.Fatal(err)
		}
		checkReady(0)
		result, err := s.Maintain(t.Context())
		if err != nil || result.SealedBlocks != 0 {
			t.Fatalf("unsafe maintenance: %+v, %v", result, err)
		}
	}
	point := Sample{At: testEpoch + 600*10000, Value: 600}
	if err := s.Ingest(t.Context(), []Batch{{Series: series, Samples: []Sample{point}}}); err != nil {
		t.Fatal(err)
	}
	checkReady(1)
	result, err := s.Maintain(t.Context())
	if err != nil || result.SealedBlocks != 1 {
		t.Fatalf("safe prefix was not sealed: %+v, %v", result, err)
	}
}
