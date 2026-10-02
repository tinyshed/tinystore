package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"math"
	"testing"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

type headReadTrace struct {
	sqlite.Reader
	materialized bool
}

func (r *headReadTrace) QueryContext(ctx context.Context, query string, arguments ...any) (*sql.Rows, error) {
	if query == headTailsQuery || query == selectedGroupsQuery {
		r.materialized = true
	}
	return r.Reader.QueryContext(ctx, query, arguments...)
}

func TestBatchedHeadsPreserveSnapshotAndReserveBytesFirst(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	const seriesCount = 20
	points := testSamples(241)
	batches := make([]Batch, seriesCount)
	for i := range batches {
		batches[i] = Batch{
			Series:  Series{Name: "cpu", Labels: Labels{"host": fmt.Sprint(i)}},
			Samples: points,
		}
	}
	if err := s.Ingest(t.Context(), batches); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	result, err := s.Read(t.Context(), Range{Name: "cpu", From: testEpoch, To: testEpoch + 241})
	if err != nil {
		t.Fatal(err)
	}
	if len(result) != seriesCount {
		t.Fatalf("read %d series", len(result))
	}
	for _, series := range result {
		if len(series.Samples) != len(points) {
			t.Fatalf("read %d samples", len(series.Samples))
		}
		for i, point := range series.Samples {
			if point.At != points[i].At || math.Float64bits(point.Value) != math.Float64bits(points[i].Value) {
				t.Fatalf("sample %d changed", i)
			}
		}
	}
	matched := make([]registeredSeries, seriesCount)
	for i := range matched {
		matched[i].id = int64(i + 1)
	}
	if err = s.file.ViewPrepared(t.Context(), func(reader sqlite.Reader) error {
		trace := &headReadTrace{Reader: reader}
		budget := queryBudget{limits: Limits{PayloadBytes: 1, DecodedSamples: 1000}}
		snapshot := snapshotRead{tx: trace, from: testEpoch, to: testEpoch + 241, budget: &budget}
		_, readErr := s.fetchHeads(t.Context(), snapshot, matched)
		if !errors.Is(readErr, ErrLimit) || trace.materialized {
			t.Fatalf("head bytes fetched before budget: %v, materialized=%v", readErr, trace.materialized)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if err = s.file.ViewPrepared(t.Context(), func(reader sqlite.Reader) error {
		trace := &headReadTrace{Reader: reader}
		budget := queryBudget{limits: Limits{PayloadBytes: 1, Blocks: 1000}}
		snapshot := snapshotRead{tx: trace, from: testEpoch, to: testEpoch + 241, budget: &budget}
		_, readErr := fetchGroupRows(t.Context(), snapshot, matched)
		if !errors.Is(readErr, ErrLimit) || trace.materialized {
			t.Fatalf("directory bytes fetched before budget: %v, materialized=%v", readErr, trace.materialized)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if err = s.file.Update(t.Context(), func(tx *sql.Tx) error {
		var id int64
		if readErr := tx.QueryRowContext(t.Context(), `select id from payloads limit 1`).Scan(&id); readErr != nil {
			return readErr
		}
		_, deleteErr := tx.ExecContext(t.Context(), `delete from payloads where id=?`, id)
		return deleteErr
	}); err != nil {
		t.Fatal(err)
	}
	result, err = s.Read(t.Context(), Range{Name: "cpu", From: testEpoch, To: testEpoch + 241})
	if !errors.Is(err, ErrCorrupt) || result != nil {
		t.Fatalf("missing batched payload: %d results, %v", len(result), err)
	}
}
