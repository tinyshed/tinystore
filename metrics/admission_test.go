package metrics

import (
	"context"
	"errors"
	"testing"
	"time"
)

func TestActiveReadAndIngestAdmissionHonorsCancellation(t *testing.T) {
	s, _ := openTestStore(t, Options{MaxConcurrentReads: 1, MaxConcurrentIngest: 1})
	series := testSeries()
	request := Range{Matchers: series.Labels, From: testEpoch, To: testEpoch + 1}
	batch := []Batch{{Series: series, Samples: testSamples(1)}}
	s.readSlots <- struct{}{}
	readCtx, cancelRead := context.WithTimeout(t.Context(), 100*time.Millisecond)
	defer cancelRead()
	if _, err := s.Read(readCtx, request); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("read admission: %v", err)
	}
	<-s.readSlots
	s.ingestSlots <- struct{}{}
	ingestCtx, cancelIngest := context.WithTimeout(t.Context(), 100*time.Millisecond)
	defer cancelIngest()
	if err := s.Ingest(ingestCtx, batch); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("ingest admission: %v", err)
	}
	<-s.ingestSlots
	if err := s.Ingest(t.Context(), batch); err != nil {
		t.Fatalf("ingest after admission: %v", err)
	}
	if _, err := s.Read(t.Context(), request); err != nil {
		t.Fatalf("read after admission: %v", err)
	}
}
