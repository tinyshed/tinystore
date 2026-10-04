package metrics

import (
	"context"
)

// Latest returns the newest sample of each series inside the range, which
// bounds how old the sample may be: a series without one is left out. It
// decodes one head chunk or one block a series at most, and no block when its
// directory holds the last sample.
func (s *Store) Latest(ctx context.Context, request Range) ([]Result, error) {
	release, err := s.admit(ctx, s.readSlots)
	if err != nil {
		return nil, err
	}
	defer release()

	query, err := s.checkRange(request)
	if err != nil {
		return nil, err
	}
	if query.from >= query.to {
		return []Result{}, nil
	}
	query.latest = true

	unreserve, err := s.reserve(ctx, func() (int64, error) { return readReservation(query.limits) })
	if err != nil {
		return nil, err
	}
	defer unreserve()

	reads, err := s.fetchSnapshot(ctx, query)
	if err != nil {
		return nil, err
	}

	results, err := s.newestOfEach(ctx, query, reads)
	if err != nil {
		return nil, err
	}
	s.queried.Add(1)
	return results, nil
}

func (s *Store) newestOfEach(ctx context.Context, query rangeQuery, reads []seriesRead) ([]Result, error) {
	results := []Result{}
	for _, read := range reads {
		sample, found, err := s.newest(ctx, read, read.window.from, query.to)
		if err != nil {
			return nil, err
		}
		if !found {
			continue
		}
		if len(results) == query.limits.OutputSamples {
			return nil, limit(LimitOutputSamples, len(results)+1, query.limits.OutputSamples)
		}
		results = append(results, Result{
			Series: publicSeries(read.series.labels, read.series.kind), Samples: []Sample{sample},
		})
	}
	return results, nil
}

// newest is a series' newest sample inside [from, to): its head's when the
// head holds one, since every block is older, and otherwise its newest block's.
func (s *Store) newest(ctx context.Context, read seriesRead, from, to int64) (Sample, bool, error) {
	var newest Sample
	found := false
	keep := func(point Sample) error {
		if point.At >= from && point.At < to {
			newest, found = point, true
		}
		return nil
	}
	if err := s.visitHead(ctx, read.head, keep); err != nil || found {
		return newest, found, err
	}
	for _, block := range read.blocks { // the newest one alone
		if block.summarized {
			return Sample{At: block.head.End, Value: block.summary.last}, true, nil
		}
		if err := s.visitBlock(ctx, block, keep); err != nil {
			return newest, false, err
		}
	}
	return newest, found, nil
}

// newestChunkFrom is where a Latest read starts decoding a head: the newest
// chunk the range touches, alone, or from when it touches none
//
//	chunks    [10:00 … 10:59] [11:00 … 11:59] [12:00 … 12:20]
//	range              10:30 ────── 11:10
//	decoded                   [11:00 … 11:59]
func newestChunkFrom(chunks []headChunk, from, to int64) int64 {
	for i := len(chunks) - 1; i >= 0; i-- {
		if chunks[i].overlaps(from, to) {
			return max(from, chunks[i].header.Start)
		}
	}
	return from
}
