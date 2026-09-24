package metrics

import (
	"context"
	"database/sql"
	"errors"
	"math"
	"testing"
	"time"
)

func TestClockSharingAndLastOwnerRetention(t *testing.T) {
	store, _ := openTestStore(t, Options{Retention: time.Second})
	first := testSeries()
	second := testSeries()
	second.Labels[0].Value = "two"
	points := testSamples(800)
	for i := range points {
		points[i].Value = float64(i / 200)
	}
	if err := store.Ingest(t.Context(), []Batch{{Series: first, Samples: points}, {Series: second, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	check := func(wantClocks, wantRefs int) {
		t.Helper()
		err := store.file.View(t.Context(), func(tx *sql.Tx) error {
			var clocks, refs int
			err := tx.QueryRowContext(t.Context(), `select count(*),coalesce(sum(refs),0) from clocks`).Scan(&clocks, &refs)
			if clocks != wantClocks || refs != wantRefs {
				t.Errorf("clock ownership %d/%d want %d/%d", clocks, refs, wantClocks, wantRefs)
			}
			return err
		})
		if err != nil {
			t.Fatal(err)
		}
	}
	check(1, 2)
	result, err := store.Read(t.Context(), Range{Matchers: []Label{{Name: "__name__", Value: "cpu"}}, From: testEpoch, To: testEpoch + 800})
	if err != nil || len(result) != 2 {
		t.Fatal("shared read", err)
	}
	for _, series := range result {
		assertSamples(t, series.Samples, points)
	}
	if _, _, err = store.expireSeries(t.Context(), 1, testEpoch+900); err != nil {
		t.Fatal(err)
	}
	check(1, 1)
	result, err = store.Read(t.Context(), Range{Matchers: second.Labels, From: testEpoch, To: testEpoch + 800})
	if err != nil {
		t.Fatal(err)
	}
	assertSamples(t, result[0].Samples, points)
	if _, _, err = store.expireSeries(t.Context(), 2, testEpoch+900); err != nil {
		t.Fatal(err)
	}
	check(0, 0)
}

func TestClockCorruptionIsRefused(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: testSamples(500)}}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := store.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), `update clocks set body=zeroblob(20)`)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	_, err := store.Read(context.Background(), Range{Matchers: testSeries().Labels, From: 0, To: math.MaxInt64})
	if !errors.Is(err, ErrCorrupt) {
		t.Fatal("clock corruption", err)
	}
}
