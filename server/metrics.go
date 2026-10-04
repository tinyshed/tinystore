package server

import (
	"context"
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
	methods[wire.MetricsExplain] = metricsExplain
	methods[wire.MetricsLatest] = metricsLatest
	methods[wire.MetricsDescribe] = metricsDescribe
	methods[wire.MetricsDescribed] = metricsDescribed
}

// metricsDescribe keeps what a metric's name means in place of what it meant
func metricsDescribe(c *call) error {
	var ask wire.MetricsDescription
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	store, err := c.session.server.metricsStore(c.ctx)
	if err != nil {
		return err
	}
	if err = store.Describe(c.ctx, ask.Name, metrics.Unit(ask.Unit), metrics.Help(ask.Help)); err != nil {
		return err
	}
	return respond(c, wire.Empty{})
}

// metricsDescribed answers what describe kept for a name, or none
func metricsDescribed(c *call) error {
	var ask wire.MetricsDescription
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	store, err := c.session.server.metricsStore(c.ctx)
	if err != nil {
		return err
	}
	description, err := store.Description(c.ctx, ask.Name)
	if err != nil {
		return err
	}
	return respond(c, wire.MetricsDescription{Name: ask.Name, Unit: description.Unit, Help: description.Help})
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
		batches[i] = metrics.Batch{Series: seriesOf(sent.Labels, sent.Kind), Samples: samples}
	}
	if err = store.Ingest(c.ctx, batches); err != nil {
		return err
	}
	return respond(c, wire.Empty{})
}

func metricsRead(c *call) error { return downloadSeries(c, (*metrics.Store).Read) }

// metricsLatest is read's download of each series' newest sample in the range
func metricsLatest(c *call) error { return downloadSeries(c, (*metrics.Store).Latest) }

type seriesReader func(*metrics.Store, context.Context, metrics.Range) ([]metrics.Result, error)

// downloadSeries answers a range with the series read finds: a series a DATA,
// or several for one whose samples pass what a body holds, then {}. The range
// is read whole, within its limits, before the first series leaves, so that a
// slow client holds none of the engine's readers.
func downloadSeries(c *call, read seriesReader) error {
	var ask wire.MetricsRange
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	store, err := c.session.server.metricsStore(c.ctx)
	if err != nil {
		return err
	}
	request, err := rangeOf(ask)
	if err != nil {
		return err
	}
	results, err := read(store, c.ctx, request)
	if err != nil {
		return err
	}
	release, err := c.holdAnswer(func() int64 { return weighSeries(results) })
	if err != nil {
		return err
	}
	defer release()

	if err = begin(c, wire.Empty{}); err != nil {
		return err
	}
	for _, result := range results {
		labels, kind := wireLabels(result.Series), string(result.Series.Kind)
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
	request, err := aggregateOf(ask)
	if err != nil {
		return err
	}
	results, err := store.Aggregate(c.ctx, request)
	if err != nil {
		return err
	}
	release, err := c.holdAnswer(func() int64 { return weighBuckets(results) })
	if err != nil {
		return err
	}
	defer release()

	if err = begin(c, wire.Empty{}); err != nil {
		return err
	}
	for _, result := range results {
		labels, kind := wireLabels(result.Series), string(result.Series.Kind)
		err = inPieces(result.Buckets, c.itemsInABody(5*8+1), func(buckets []metrics.AggregateBucket) error {
			sent := wire.MetricsBuckets{Labels: labels, Kind: kind, Buckets: make([]wire.MetricsBucket, len(buckets))}
			for i, bucket := range buckets {
				sent.Buckets[i] = wire.MetricsBucket{
					From: bucket.From, To: bucket.To, Count: int64(bucket.Count), Resets: int64(bucket.Resets),
					Value: bucket.Value, Overflow: bucket.Overflow, Partial: bucket.Partial, Lookback: bucket.Lookback,
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
	series := seriesOf(ask.Labels, "")
	dropped, err := store.DropSeries(c.ctx, series.Name, series.Labels)
	if err != nil {
		return err
	}
	unreadable := uint64(max(dropped.UnreadableGroups, 0))
	return respond(c, wire.MetricsDropped{Found: dropped.Found, UnreadableGroups: unreadable})
}

// itemsInABody is how many items of size bytes each fit half the body the
// client agreed, the other half being left to the series' labels. A sample is
// its time and its value; a bucket is four integers, a value and a byte of
// flags.
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
func rangeOf(sent wire.MetricsRange) (metrics.Range, error) {
	matched := seriesOf(sent.Matchers, "")
	where, err := whereOf(sent.Where)
	if err != nil {
		return metrics.Range{}, err
	}
	return metrics.Range{
		Name: matched.Name, Match: matched.Labels, Where: where, From: sent.From, To: sent.To,
		Limits: metrics.Limits{
			Series: limitOf(sent.Limits.Series), Blocks: limitOf(sent.Limits.Blocks),
			PayloadBytes: limitOf(sent.Limits.PayloadBytes), DecodedSamples: limitOf(sent.Limits.DecodedSamples),
			OutputSamples: limitOf(sent.Limits.OutputSamples),
		},
	}, nil
}

// aggregateOf is an aggregate as the engine takes it: its range, its buckets'
// width and lookback, an operation this server has, and its grouping
func aggregateOf(ask wire.MetricsRange) (metrics.AggregateRequest, error) {
	if ask.Width > math.MaxInt64/int64(time.Millisecond) {
		return metrics.AggregateRequest{}, fmt.Errorf("%w: metrics: buckets %d milliseconds wide, past what a "+
			"duration holds", tinystore.ErrInvalid, ask.Width)
	}
	if ask.Lookback > math.MaxInt64/int64(time.Millisecond) {
		return metrics.AggregateRequest{}, fmt.Errorf("%w: metrics: a lookback of %d milliseconds, past what a "+
			"duration holds", tinystore.ErrInvalid, ask.Lookback)
	}
	selected, err := rangeOf(ask)
	if err != nil {
		return metrics.AggregateRequest{}, err
	}
	if ask.Op != "" && !slices.Contains(aggregateOps, metrics.AggregateOp(ask.Op)) {
		return metrics.AggregateRequest{}, unknownKind("an aggregate operation", "op", ask.Op)
	}
	return metrics.AggregateRequest{
		Range: selected, Width: time.Duration(ask.Width) * time.Millisecond, Op: metrics.AggregateOp(ask.Op),
		By: ask.By, Without: ask.Without, Lookback: time.Duration(ask.Lookback) * time.Millisecond,
	}, nil
}

// metricsExplain answers what a read, or an aggregate when the range names an
// operation, would spend, without a payload fetched or a sample decoded
func metricsExplain(c *call) error {
	var ask wire.MetricsRange
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	store, err := c.session.server.metricsStore(c.ctx)
	if err != nil {
		return err
	}
	var plan metrics.Plan
	if ask.Op == "" {
		selected, rangeErr := rangeOf(ask)
		if rangeErr != nil {
			return rangeErr
		}
		plan, err = store.ExplainRead(c.ctx, selected)
	} else {
		request, requestErr := aggregateOf(ask)
		if requestErr != nil {
			return requestErr
		}
		plan, err = store.ExplainAggregate(c.ctx, request)
	}
	if err != nil {
		return err
	}
	sent := wire.MetricsPlan{
		Series: counted(plan.Series), Blocks: counted(plan.Blocks), Summarized: counted(plan.Summarized),
		Bytes: counted(plan.PayloadBytes), Decoded: counted(plan.DecodedSamples),
		Limits: wire.MetricsLimits{
			Series: counted(plan.Limits.Series), Blocks: counted(plan.Limits.Blocks),
			PayloadBytes: counted(plan.Limits.PayloadBytes), DecodedSamples: counted(plan.Limits.DecodedSamples),
			OutputSamples: counted(plan.Limits.OutputSamples),
		},
	}
	if plan.Stops != nil {
		sent.Stops = failure(c.ctx, plan.Stops)
	}
	return respond(c, sent)
}

// counted is a count as the wire carries it, never below zero
func counted(n int) uint64 {
	return uint64(max(n, 0))
}

// the operations an aggregate may ask for; another is a newer client's
var aggregateOps = []metrics.AggregateOp{
	metrics.AggregateCount, metrics.AggregateSum, metrics.AggregateMin, metrics.AggregateMax, metrics.AggregateAvg,
	metrics.AggregateIncrease, metrics.AggregateRate, metrics.AggregateDelta, metrics.AggregateFirst,
	metrics.AggregateLast,
}

// unknownKind refuses a value of a known field that this server does not have,
// a newer client's, as it refuses a field it does not know: answering without
// it would answer another question.
//
//	unknownKind("an aggregate operation", "op", "median")
//	  → unimplemented: metrics: an aggregate operation "median", which this server does not have
func unknownKind(what, field, kind string) error {
	return &wire.Error{
		Code: wire.CodeUnimplemented, What: map[string]string{field: kind},
		Message: fmt.Sprintf("metrics: %s %q, which this server does not have", what, kind),
	}
}

// whereOf is the wire's conditions as the engine takes them, a kind it does
// not have refused, since skipping it would answer another question
func whereOf(sent []wire.MetricsCondition) (metrics.Where, error) {
	if len(sent) == 0 {
		return nil, nil
	}
	where := make(metrics.Where, len(sent))
	for _, condition := range sent {
		name := condition.Label
		if _, twice := where[name]; twice {
			return nil, fmt.Errorf("%w: metrics: label %q has two conditions", tinystore.ErrInvalid, name)
		}
		switch condition.Kind {
		case "one_of":
			where[name] = metrics.OneOf(condition.Values...)
		case "none_of":
			where[name] = metrics.NoneOf(condition.Values...)
		case "prefix":
			if len(condition.Values) != 1 {
				return nil, fmt.Errorf("%w: metrics: label %q: a prefix is one value", tinystore.ErrInvalid, name)
			}
			where[name] = metrics.Prefix(condition.Values[0])
		default:
			return nil, unknownKind("a condition of kind", "condition", condition.Kind)
		}
	}
	return where, nil
}

func limitOf(limit uint64) int {
	return int(min(limit, math.MaxInt32))
}

// wireName is the label the wire carries a series' name as, beside its
// labels, as the store keeps it
const wireName = "__name__"

// seriesOf is a series as the wire spells it, its name among its labels
func seriesOf(labels map[string]string, kind string) metrics.Series {
	series := metrics.Series{Name: labels[wireName], Kind: metrics.Kind(kind)}
	series.Labels = make(metrics.Labels, len(labels))
	for name, value := range labels {
		if name != wireName {
			series.Labels[name] = value
		}
	}
	return series
}

// wireLabels is a series' name and labels as the wire spells them
func wireLabels(series metrics.Series) map[string]string {
	spelled := make(map[string]string, len(series.Labels)+1)
	maps.Copy(spelled, series.Labels)
	if series.Name != "" {
		spelled[wireName] = series.Name
	}
	return spelled
}
