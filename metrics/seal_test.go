package metrics

import (
	"database/sql"
	"testing"
	"time"
)

func TestWatermarkIsStrictAndFollowsTheSeries(t *testing.T) {
	store, _ := openTestStore(t, Options{Lateness: 261 * time.Millisecond})
	points := append(testSamples(240), Sample{At: testEpoch + 500, Value: 1})
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
		t.Fatal(err)
	}
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 3600000) }
	work, err := store.Maintain(t.Context())
	if err != nil || work.SealedBlocks != 0 {
		t.Fatalf("sealed the watermark or followed wall time: %+v %v", work, err)
	}
	if err = store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: []Sample{{At: testEpoch + 501, Value: 2}}}}); err != nil {
		t.Fatal(err)
	}
	work, err = store.Maintain(t.Context())
	if err != nil || work.SealedBlocks != 1 {
		t.Fatalf("new sample did not release prefix: %+v %v", work, err)
	}
}

func TestReadyWaitsForASealableWatermarkPrefix(t *testing.T) {
	s, _ := openTestStore(t, Options{Lateness: time.Hour, MaintenanceSeries: 1})
	s.now = func() time.Time { return time.UnixMilli(testEpoch + 600*10000) }
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

func TestBlockSpanIsBoundedWithoutDroppingThePrefix(t *testing.T) {
	store, _ := openTestStore(t, Options{MaxBlockSpan: 10 * time.Millisecond})
	points := testSamples(500)
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
		group, _, err := store.firstGroup(t.Context(), tx, 1)
		if err != nil {
			return err
		}
		for _, b := range group.blocks {
			if b.head.End-b.head.Start > 10 {
				t.Fatal("block span")
			}
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	assertSamples(t, readAll(t, store), points)
}
