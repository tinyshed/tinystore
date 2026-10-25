// Package metrics is numbers that move over time, as docs/guides/metrics/README.md has
// them: series by their name and labels, written whole by Ingest and read back
// by range, in buckets and as their newest samples; what a store holds of
// them; and a program's instruments, whose totals the store keeps. A sample
// is kept bit for bit: -0, infinities and a NaN's payload come back as they
// went in.
//
//	requests, err := metrics.OpenCounter(store, "http_requests_total", metrics.Help("Requests served."))
//	requests.With("route", "/notes", "status", 200).Inc()
//	traffic, err := metrics.Aggregate(ctx, store, metrics.Query{Name: "http_requests_total", Since: 24 * time.Hour,
//		Every: time.Hour, Op: metrics.Increase, By: []string{"route"}})
package metrics

import (
	"context"
	"encoding/binary"
	"math"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// Labels are a series' labels, by name.
type Labels map[string]string

// Kind is what a series' samples are.
type Kind string

const (
	KindGauge   Kind = "gauge"
	KindCounter Kind = "counter"
	// KindHistogram is the _bucket series of a histogram written elsewhere, each with its le.
	KindHistogram Kind = "histogram"
)

// Sample is a time, kept to the millisecond, and a number, kept bit for bit.
type Sample struct {
	At    time.Time
	Value float64
}

// Series is a series: what Ingest writes, and what a read gives back. Ingest
// takes its samples as Samples, or as Times and Values, two columns of one
// length; a read gives Samples.
type Series struct {
	Name    string
	Kind    Kind
	Labels  Labels
	Samples []Sample
	Times   []time.Time
	Values  []float64
	// Keep is how long the name's samples stay, its own, kept in the file;
	// what it keeps already unless given.
	Keep time.Duration
}

// Ingest writes series and their samples, every one or none: a refused
// series names itself by its labels. It returns once they are written.
func Ingest(ctx context.Context, store *tinystore.Store, series ...Series) error {
	message := wire.MetricsIngest{Series: make([]wire.MetricsNewSeries, len(series))}
	for i, each := range series {
		written, err := newSeries(each)
		if err != nil {
			return err
		}
		message.Series[i] = written
	}
	_, err := linkOf(store).Call(ctx, core.Writing, wire.MethodMetricsIngest, wire.Encode(&message))
	return err
}

// IngestPrometheus reads the text an exporter serves, in Prometheus' format
// or OpenMetrics', and writes its series, every one or none: a counter as a
// counter, a histogram's _bucket series as KindHistogram. It is told Labels
// that go beside every series' own, At, the time of a sample the text gives
// none, the store's clock unless given, and Keep, how long every name of the
// text keeps its samples. A line it cannot read is an Invalid error naming
// the line.
//
//	body, err := io.ReadAll(resp.Body)
//	err = metrics.IngestPrometheus(ctx, store, body, metrics.Labels{"instance": "web-1"})
func IngestPrometheus(ctx context.Context, store *tinystore.Store, text []byte, opts ...Option) error {
	var t told
	for _, opt := range opts {
		opt.instrument(&t)
	}
	switch {
	case t.refused != nil:
		return core.Wrap(core.Invalid, t.refused, "metrics")
	case t.unit != nil || t.help != nil || t.bounds != nil || t.exponential:
		return core.Errorf(core.Invalid, "metrics: a text's ingest is told Labels, At and Keep alone")
	}
	message := wire.MetricsExposition{Text: text, Labels: t.labels, Keep: t.keep}
	if t.at != nil {
		at := t.at.UnixMilli()
		message.At = &at
	}
	_, err := linkOf(store).Call(ctx, core.Writing, wire.MethodMetricsIngestPrometheus, wire.Encode(&message))
	return err
}

func newSeries(given Series) (wire.MetricsNewSeries, error) {
	out := wire.MetricsNewSeries{Name: given.Name, Kind: string(given.Kind), Labels: given.Labels}
	times, values := given.Times, given.Values
	switch {
	case given.Samples != nil && (times != nil || values != nil):
		return out, core.Errorf(core.Invalid, "metrics %s: samples, or times and values, not both", given.Name)
	case given.Samples != nil:
		times, values = make([]time.Time, len(given.Samples)), make([]float64, len(given.Samples))
		for i, sample := range given.Samples {
			times[i], values[i] = sample.At, sample.Value
		}
	case len(times) != len(values):
		return out, core.Errorf(core.Invalid, "metrics %s: samples, or times and values as long as each other", given.Name)
	}
	out.Times, out.Values = make([]byte, 8*len(times)), make([]byte, 8*len(values))
	for i, at := range times {
		binary.LittleEndian.PutUint64(out.Times[8*i:], uint64(at.UnixMilli()))
		binary.LittleEndian.PutUint64(out.Values[8*i:], math.Float64bits(values[i]))
	}
	if given.Keep != 0 {
		ms, err := core.Millis(given.Keep, "metrics "+given.Name+": keep")
		if err != nil {
			return out, err
		}
		out.Keep = &ms
	}
	return out, nil
}

// Condition is what a label's value must be beside a value of its own: one
// of a list, none of one, or a prefix.
type Condition struct{ wire wire.MetricsCondition }

// OneOf is a label equal to one of these.
func OneOf(values ...any) Condition {
	return Condition{wire.MetricsCondition{Values: texts(values)}}
}

// Not is a label equal to none of these, or absent.
func Not(values ...any) Condition {
	return Condition{wire.MetricsCondition{Values: texts(values), Not: true}}
}

// Prefix is a label that starts with this text, in a query; in a listing,
// the names, labels or values that do.
func Prefix(text string) Condition {
	return Condition{wire.MetricsCondition{Prefix: &text}}
}

func texts(values []any) []string {
	out := make([]string, len(values))
	for i, value := range values {
		out[i], _ = labelText(value)
	}
	return out
}

// Where is what a query finds of each label: a value, a number or a bool as
// its text, or a Condition.
type Where map[string]any

// Op is what an aggregate's bucket answers.
type Op string

const (
	Count    Op = "count"
	Sum      Op = "sum"
	Avg      Op = "avg"
	Min      Op = "min"
	Max      Op = "max"
	Increase Op = "increase"
	// Rate is a second's increase; a timer's durations a second.
	Rate  Op = "rate"
	Delta Op = "delta"
	First Op = "first"
	Last  Op = "last"
)

// Percentile is a gauge's sample at this rank, or a timer's bucket of its
// spread that holds it: Percentile(95), Percentile(99.9).
func Percentile(rank float64) Op { return Op("p" + numberText(rank)) }

// Query is the series a call finds and its range, and for an aggregate how
// it cuts and groups them. A query needs a Name, or a label's value, a list or
// a prefix in Where.
type Query struct {
	// Name is the series of this name; a timer's name, the series it writes.
	Name  string
	Where Where
	// Since is the span before now the range covers; or From and To.
	Since time.Duration
	// From is the range's start, included, and To its end, excluded: now,
	// a sample taken now in the range, when zero.
	From, To time.Time

	// Every is the buckets' span, counted from 1970 in UTC; the range is one bucket without it.
	Every time.Duration
	// Calendar is buckets of a calendar in TimeZone, in place of Every:
	// tinystore.Day begins each at a local midnight.
	Calendar tinystore.Calendar
	// TimeZone is the IANA zone a calendar's buckets are in: "Europe/Moscow", "UTC".
	TimeZone string
	Op       Op
	// By is the labels a group keeps; without By and Without, every series of a name is one group.
	By []string
	// Without is the labels a group leaves out, keeping every other.
	Without []string
	// Estimate gives a timer's percentile a point between its bounds, interpolated.
	Estimate bool
}

func (q Query) asked() (wire.MetricsQuery, error) {
	out := wire.MetricsQuery{Where: map[string]wire.MetricsCondition{}}
	if q.Name != "" {
		out.Name = &q.Name
	}
	if q.Since != 0 && !q.From.IsZero() {
		return out, core.Errorf(core.Invalid, "a query starts since a span before now or from a time, not both")
	}
	if q.Since != 0 {
		ms, err := core.Millis(q.Since, "a query's since")
		if err != nil {
			return out, err
		}
		out.Since = &ms
	}
	if !q.From.IsZero() {
		from := q.From.UnixMilli()
		out.From = &from
	}
	if !q.To.IsZero() {
		to := q.To.UnixMilli()
		out.To = &to
	}
	for label, given := range q.Where {
		switch condition := given.(type) {
		case nil:
			return out, core.Errorf(core.Invalid, "where %s: a condition given as nil", label)
		case Condition:
			out.Where[label] = condition.wire
		default:
			text, err := labelText(given)
			if err != nil {
				return out, core.Wrap(core.Invalid, err, "where %s", label)
			}
			out.Where[label] = wire.MetricsCondition{Values: []string{text}}
		}
	}
	return out, nil
}

func (q Query) aggregate() (wire.MetricsAggregateAsked, error) {
	query, err := q.asked()
	out := wire.MetricsAggregateAsked{Query: query, Estimate: q.Estimate}
	if err != nil {
		return out, err
	}
	if q.Every != 0 {
		ms, err := core.Millis(q.Every, "an aggregate's every")
		if err != nil {
			return out, err
		}
		out.Every = &ms
	}
	out.Calendar, out.TimeZone = core.CalendarOf(string(q.Calendar), q.TimeZone)
	if q.Op != "" {
		op := string(q.Op)
		out.Op = &op
	}
	if q.By != nil {
		out.By = &q.By
	}
	if q.Without != nil {
		out.Without = &q.Without
	}
	return out, nil
}

func linkOf(store *tinystore.Store) *core.Link { return store.CoreHome().Store.Link }

// download runs a method whose answer comes in parts, and gives each part.
func download(ctx context.Context, store *tinystore.Store, method uint16, body []byte) ([][]byte, error) {
	var parts [][]byte
	err := linkOf(store).Run(ctx, core.Reading, func(c *core.Connection) error {
		_, items, trailer, err := core.Download(ctx, c, method, body)
		parts = append(items, trailer)
		return err
	})
	return parts, err
}

// Read is the samples of each series the query finds, in its range: whole,
// or a Limit error naming its bound, never fewer.
func Read(ctx context.Context, store *tinystore.Store, q Query) ([]Series, error) {
	asked, err := q.asked()
	if err != nil {
		return nil, err
	}
	parts, err := download(ctx, store, wire.MethodMetricsRead, wire.Encode(&asked))
	if err != nil {
		return nil, err
	}
	var series []Series
	more := false
	for _, part := range parts {
		var read wire.MetricsRead
		if err := decode(part, &read); err != nil {
			return nil, err
		}
		for _, piece := range read.Series {
			samples := samplesOf(piece.Times, piece.Values)
			if more && len(series) > 0 {
				series[len(series)-1].Samples = append(series[len(series)-1].Samples, samples...)
			} else {
				series = append(series, Series{Name: piece.Name, Kind: Kind(piece.Kind), Labels: piece.Labels, Samples: samples})
			}
			more = piece.More
		}
	}
	return series, nil
}

// samplesOf is two columns as samples, each value's bits as they came.
func samplesOf(times, values []byte) []Sample {
	samples := make([]Sample, min(len(times), len(values))/8)
	for i := range samples {
		at := int64(binary.LittleEndian.Uint64(times[8*i:]))
		samples[i] = Sample{At: time.UnixMilli(at).UTC(), Value: math.Float64frombits(binary.LittleEndian.Uint64(values[8*i:]))}
	}
	return samples
}

// Point is a series' newest sample in the range.
type Point struct {
	Name   string
	Labels Labels
	At     time.Time
	Value  float64
}

// Latest is the newest sample of each series the query finds, when it has
// one in the range: a series without one is left out.
func Latest(ctx context.Context, store *tinystore.Store, q Query) ([]Point, error) {
	asked, err := q.asked()
	if err != nil {
		return nil, err
	}
	parts, err := download(ctx, store, wire.MethodMetricsLatest, wire.Encode(&asked))
	if err != nil {
		return nil, err
	}
	var points []Point
	for _, part := range parts {
		var latest wire.MetricsLatest
		if err := decode(part, &latest); err != nil {
			return nil, err
		}
		for _, each := range latest.Series {
			points = append(points, Point{Name: each.Name, Labels: each.Labels, At: time.UnixMilli(each.At).UTC(), Value: each.Value})
		}
	}
	return points, nil
}

// Bucket is a bucket of an aggregate. One of count … last has its Value; a
// percentile's is a gauge's sample at the rank, or a timer's bucket of its
// spread that holds it, from Lower to Upper, and with Estimate asked a point
// between them.
type Bucket struct {
	// At is where it begins.
	At time.Time
	// Value is none for a timer's percentile.
	Value *float64
	Lower *float64
	// Upper is +Inf past the last bound.
	Upper    *float64
	Estimate *float64
	// Samples is how many samples it comes from; a timer's, how many durations.
	Samples int
	Resets  int
	// Partial says its value comes from less than its span: the range, the
	// clock or retention cut it.
	Partial bool
	// Gap says its first step began before the bucket before it: a whole
	// bucket between had no samples.
	Gap bool
}

// Group is the buckets of a group of series: a bucket without samples is left out.
type Group struct {
	Name string
	// Labels is the labels By kept or Without left.
	Labels  Labels
	Buckets []Bucket
}

// Aggregate is the buckets of each group of the series the query finds,
// each one exact value of its Op, rounded once.
func Aggregate(ctx context.Context, store *tinystore.Store, q Query) ([]Group, error) {
	if q.Op == "" {
		return nil, core.Errorf(core.Invalid, "an aggregate takes an op: count, sum, avg, min, max, increase, rate, delta, first, last, a percentile")
	}
	asked, err := q.aggregate()
	if err != nil {
		return nil, err
	}
	parts, err := download(ctx, store, wire.MethodMetricsAggregate, wire.Encode(&asked))
	if err != nil {
		return nil, err
	}
	var groups []Group
	more := false
	for _, part := range parts {
		var read wire.MetricsAggregates
		if err := decode(part, &read); err != nil {
			return nil, err
		}
		for _, piece := range read.Groups {
			buckets := make([]Bucket, len(piece.Buckets))
			for i, bucket := range piece.Buckets {
				buckets[i] = bucketOf(bucket)
			}
			if more && len(groups) > 0 {
				groups[len(groups)-1].Buckets = append(groups[len(groups)-1].Buckets, buckets...)
			} else {
				groups = append(groups, Group{Name: piece.Name, Labels: piece.Labels, Buckets: buckets})
			}
			more = piece.More
		}
	}
	return groups, nil
}

func bucketOf(bucket wire.MetricsBucket) Bucket {
	out := Bucket{
		At: time.UnixMilli(bucket.At).UTC(), Value: bucket.Value, Lower: bucket.Lower, Upper: bucket.Upper,
		Estimate: bucket.Estimate, Samples: int(bucket.Samples), Resets: int(bucket.Resets), Partial: bucket.Partial, Gap: bucket.Gap,
	}
	if out.Lower != nil && out.Upper == nil {
		infinity := math.Inf(1)
		out.Upper = &infinity
	}
	return out
}

// Exceeds is the bound a call would stop at, as its Limit error names it.
type Exceeds struct {
	// Limit is series, bytes read, samples decoded or answer.
	Limit         string
	Wanted, Bound int64
}

// Plan is what a read or an aggregate would spend of each bound.
type Plan struct {
	Series, Bytes, Decoded int64
	// Exceeds is nil when the call fits every bound.
	Exceeds *Exceeds
}

// Explain is what Read, or Aggregate when given an Op, would spend of each
// bound; it decodes no sample.
func Explain(ctx context.Context, store *tinystore.Store, q Query) (Plan, error) {
	asked, err := q.aggregate()
	if err != nil {
		return Plan{}, err
	}
	answer, err := linkOf(store).Call(ctx, core.Reading, wire.MethodMetricsExplain, wire.Encode(&asked))
	if err != nil {
		return Plan{}, err
	}
	var plan wire.MetricsPlan
	if err := decode(answer, &plan); err != nil {
		return Plan{}, err
	}
	out := Plan{Series: int64(plan.Series), Bytes: int64(plan.Bytes), Decoded: int64(plan.Decoded)}
	if plan.Exceeds != nil {
		out.Exceeds = &Exceeds{Limit: plan.Exceeds.Limit, Wanted: int64(plan.Exceeds.Wanted), Bound: int64(plan.Exceeds.Bound)}
	}
	return out, nil
}

func decode(answer []byte, m wire.Message) error {
	if err := wire.Decode(answer, m); err != nil {
		return core.Wrap(core.Protocol, err, "an answer that does not read")
	}
	return nil
}
