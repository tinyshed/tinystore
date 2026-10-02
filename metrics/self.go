package metrics

import (
	"context"
	"fmt"

	"github.com/tinyshed/tinystore"
)

func (s *Store) Report() []tinystore.Measure {
	stats := s.Stats()
	return []tinystore.Measure{
		{Engine: "metrics", Name: "ingested_samples_total", Value: stats.IngestedSamples, Counter: true},
		{Engine: "metrics", Name: "rejected_batches_total", Value: stats.RejectedBatches, Counter: true},
		{Engine: "metrics", Name: "queries_total", Value: stats.Queries, Counter: true},
		{Engine: "metrics", Name: "sealed_blocks_total", Value: stats.SealedBlocks, Counter: true},
		{Engine: "metrics", Name: "expired_samples_total", Value: stats.ExpiredSamples, Counter: true},
		{Engine: "metrics", Name: "quarantined_series", Value: stats.QuarantinedSeries},
		{Engine: "metrics", Name: "reclaimed_series_total", Value: stats.ReclaimedSeries, Counter: true},
	}
}

// WriteSelf keeps diagnostics in ordinary series without recursively counting
// their own ingest. Caller counters above 2^53 are refused, never rounded.
func (s *Store) WriteSelf(ctx context.Context, measures []tinystore.Measure) error {
	if len(measures) > 256 {
		return fmt.Errorf("%w: self-metrics measurements", ErrLimit)
	}
	now := s.now()
	at := now.UnixMilli()
	accepted := window{
		cutoff: earlier(at, s.opts.Retention.Milliseconds()), horizon: later(at, s.opts.ClockSkew.Milliseconds()),
	}
	batches := make([]Batch, 0, len(measures))
	for _, measure := range measures {
		if len(measure.Engine) == 0 || len(measure.Engine) > 64 || len(measure.Name) == 0 || len(measure.Name) > 64 {
			return fmt.Errorf("%w: self-metrics name", ErrInvalid)
		}
		if measure.Value > 1<<53 {
			return fmt.Errorf("%w: self-metrics integer", ErrLimit)
		}
		kind := Gauge
		if measure.Counter {
			kind = Counter
		}
		batches = append(batches, Batch{
			Series:  Series{Name: "tinystore_" + measure.Name, Kind: kind, Labels: Labels{"engine": measure.Engine}},
			Samples: []Sample{{At: at, Value: float64(measure.Value)}},
		})
	}
	return s.ingest(ctx, batches, false, &accepted)
}
