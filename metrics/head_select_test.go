package metrics

import (
	"database/sql"
	"errors"
	"fmt"
	"math"
	"testing"
)

func TestNarrowPackedHeadChargesSelectedChunksAndChecksWholeChecksum(t *testing.T) {
	s, _ := openTestStore(t, Options{MaxHeadSamples: 512})
	points := testSamples(481)
	if err := s.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
		t.Fatal(err)
	}
	last := points[len(points)-1]
	request := Range{Matchers: testSeries().Labels, From: last.At, To: last.At + 1, Limits: Limits{DecodedSamples: 1, OutputSamples: 1}}
	read, err := s.Read(t.Context(), request)
	if err != nil || len(read) != 1 || len(read[0].Samples) != 1 || math.Float64bits(read[0].Samples[0].Value) != math.Float64bits(last.Value) {
		t.Fatalf("narrow last chunk: %+v, %v", read, err)
	}
	firstChunk := Range{Matchers: testSeries().Labels, From: points[1].At, To: points[1].At + 1, Limits: Limits{DecodedSamples: 239}}
	if read, err := s.Read(t.Context(), firstChunk); !errors.Is(err, ErrLimit) || read != nil {
		t.Fatalf("undersized selected chunk budget: %+v, %v", read, err)
	}
	firstChunk.Limits.DecodedSamples = 240
	if read, err := s.Read(t.Context(), firstChunk); err != nil || len(read) != 1 || len(read[0].Samples) != 1 {
		t.Fatalf("selected first chunk: %+v, %v", read, err)
	}
	if err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
		var tail []byte
		if err := tx.QueryRowContext(t.Context(), `select tail from series_state where series_id=1`).Scan(&tail); err != nil {
			return err
		}
		tail[6] ^= 0xff
		_, err := tx.ExecContext(t.Context(), `update series_state set tail=? where series_id=1`, tail)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if read, err := s.Read(t.Context(), request); !errors.Is(err, ErrCorrupt) || read != nil {
		t.Fatalf("unselected prefix corruption: %+v, %v", read, err)
	}
}

func TestBatchedNarrowHeadsChargeSelectedChunks(t *testing.T) {
	s, _ := openTestStore(t, Options{MaxHeadSamples: 512})
	points := testSamples(481)
	batches := make([]Batch, 20)
	for i := range batches {
		batches[i] = Batch{Series: Series{Labels: []Label{{Name: "__name__", Value: "cpu"}, {Name: "host", Value: fmt.Sprint(i)}}}, Samples: points}
	}
	if err := s.Ingest(t.Context(), batches); err != nil {
		t.Fatal(err)
	}
	last := points[len(points)-1]
	request := Range{Matchers: []Label{{Name: "__name__", Value: "cpu"}}, From: last.At, To: last.At + 1, Limits: Limits{DecodedSamples: 20, OutputSamples: 20}}
	read, err := s.Read(t.Context(), request)
	if err != nil || len(read) != 20 {
		t.Fatalf("batched narrow heads: %d series, %v", len(read), err)
	}
	for _, series := range read {
		if len(series.Samples) != 1 || math.Float64bits(series.Samples[0].Value) != math.Float64bits(last.Value) {
			t.Fatalf("batched narrow value changed: %+v", series)
		}
	}
}
