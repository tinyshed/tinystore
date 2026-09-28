package server

import (
	"fmt"
	"maps"
	"math"
	"slices"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/metrics"
	"github.com/tinyshed/tinystore/server/wire"
)

func (s *Server) metricsMethods(methods map[wire.Method]handler) {
	methods[wire.MetricsIngest] = metricsIngest
	methods[wire.MetricsRead] = metricsRead
	methods[wire.MetricsAggregate] = metricsAggregate
	methods[wire.MetricsDrop] = metricsDrop
}

// metricsIngest stores its series' samples, all or none
func metricsIngest(c *call) error {
	var ask wire.MetricsBatch
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	store, err := c.session.server.metricsStore(c.ctx)
	if err != nil {
		return err
	}
	batches := make([]metrics.Batch, len(ask.Series))
	for i, sent := range ask.Series {
		samples := make([]metrics.Sample, len(sent.Times))
		for j := range samples {
			samples[j] = metrics.Sample{At: sent.Times[j], Value: sent.Values[j]}
		}
		series := metrics.Series{Labels: labelsOf(sent.Labels), Kind: metrics.Kind(sent.Kind)}
		batches[i] = metrics.Batch{Series: series, Samples: samples}
	}
	if err = store.Ingest(c.ctx, batches); err != nil {
		return err
	}
	return respond(c, wire.Empty{})
}

// metricsRead is a download: a series a DATA, or several for one whose
// samples pass what a body holds, then {}. The range is read whole, within its
// limits, before the first series leaves, so that a slow client holds none of
// the engine's readers.
func metricsRead(c *call) error {
	var ask wire.MetricsRange
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	store, err := c.session.server.metricsStore(c.ctx)
	if err != nil {
		return err
	}
	results, err := store.Read(c.ctx, rangeOf(ask))
	if err != nil {
		return err
	}

	if err = begin(c, wire.Empty{}); err != nil {
		return err
	}
	for _, result := range results {
		labels, kind := labelMapOf(result.Series.Labels), string(result.Series.Kind)
		err = inPieces(result.Samples, c.itemsInABody(8+8), func(samples []metrics.Sample) error {
			sent := wire.MetricsSeries{
				Labels: labels, Kind: kind, Times: make([]int64, len(samples)),
				Values: make([]float64, len(samples)),
			}
			for i, sample := range samples {
				sent.Times[i], sent.Values[i] = sample.At, sample.Value
			}
			return item(c, sent)
		})
		if err != nil {
			return err
		}
	}
	return trailer(c, wire.Empty{})
}

// metricsAggregate is a download: a series' buckets a DATA, or several for a
// series whose buckets pass what a body holds, then {}
func metricsAggregate(c *call) error {
	var ask wire.MetricsRange
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	store, err := c.session.server.metricsStore(c.ctx)
	if err != nil {
		return err
	}
	if ask.Width > math.MaxInt64/int64(time.Millisecond) {
		return fmt.Errorf("%w: metrics: buckets %d milliseconds wide, past what a duration holds",
			tinystore.ErrInvalid, ask.Width)
	}
	request := metrics.AggregateRequest{
		Range: rangeOf(ask), Width: time.Duration(ask.Width) * time.Millisecond, Op: metrics.AggregateOp(ask.Op),
	}
	results, err := store.Aggregate(c.ctx, request)
	if err != nil {
		return err
	}

	if err = begin(c, wire.Empty{}); err != nil {
		return err
	}
	for _, result := range results {
		labels, kind := labelMapOf(result.Series.Labels), string(result.Series.Kind)
		err = inPieces(result.Buckets, c.itemsInABody(5*8+1), func(buckets []metrics.AggregateBucket) error {
			sent := wire.MetricsBuckets{Labels: labels, Kind: kind, Buckets: make([]wire.MetricsBucket, len(buckets))}
			for i, bucket := range buckets {
				sent.Buckets[i] = wire.MetricsBucket{
					From: bucket.From, To: bucket.To, Count: int64(bucket.Count), Resets: int64(bucket.Resets),
					Value: bucket.Value, Overflow: bucket.Overflow, Partial: bucket.Partial,
				}
			}
			return item(c, sent)
		})
		if err != nil {
			return err
		}
	}
	return trailer(c, wire.Empty{})
}

// metricsDrop removes one series and everything it holds
func metricsDrop(c *call) error {
	var ask wire.MetricsLabels
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	store, err := c.session.server.metricsStore(c.ctx)
	if err != nil {
		return err
	}
	dropped, err := store.DropSeries(c.ctx, labelsOf(ask.Labels))
	if err != nil {
		return err
	}
	unreadable := uint64(max(dropped.UnreadableGroups, 0))
	return respond(c, wire.MetricsDropped{Found: dropped.Found, UnreadableGroups: unreadable})
}

// itemsInABody is how many items of size bytes each fit half the body the
// client agreed, leaving the other half to the series' labels: a sample is
// its time and its value, a bucket four integers, a value and a byte of flags
func (c *call) itemsInABody(size int) int {
	return max(1, int(c.session.agreed.maxBody)/2/size)
}

// inPieces hands send items n at a time, the last piece shorter
func inPieces[T any](items []T, n int, send func([]T) error) error {
	for len(items) > 0 {
		piece := items[:min(n, len(items))]
		if err := send(piece); err != nil {
			return err
		}
		items = items[len(piece):]
	}
	return nil
}

// rangeOf is a range as the engine takes it; a limit of zero is the server's
func rangeOf(sent wire.MetricsRange) metrics.Range {
	return metrics.Range{
		Matchers: labelsOf(sent.Matchers), From: sent.From, To: sent.To,
		Limits: metrics.Limits{
			Series: limitOf(sent.Limits.Series), Blocks: limitOf(sent.Limits.Blocks),
			PayloadBytes: limitOf(sent.Limits.PayloadBytes), DecodedSamples: limitOf(sent.Limits.DecodedSamples),
			OutputSamples: limitOf(sent.Limits.OutputSamples),
		},
	}
}

func limitOf(limit uint64) int {
	return int(min(limit, math.MaxInt32))
}

// labelsOf is a map of labels as the engine takes them, ordered by name
func labelsOf(labels map[string]string) []metrics.Label {
	ordered := make([]metrics.Label, 0, len(labels))
	for _, name := range slices.Sorted(maps.Keys(labels)) {
		ordered = append(ordered, metrics.Label{Name: name, Value: labels[name]})
	}
	return ordered
}

func labelMapOf(labels []metrics.Label) map[string]string {
	named := make(map[string]string, len(labels))
	for _, label := range labels {
		named[label.Name] = label.Value
	}
	return named
}
