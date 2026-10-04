package metrics

import (
	"fmt"
	"math"
	"sort"
)

// preparedBatch is one series' samples from one call: checked, sorted, unique.
type preparedBatch struct {
	identity string
	labels   []label
	kind     Kind
	samples  []Sample
	cutoff   int64 // the oldest time the series keeps
}

// prepareIngest checks a call's batches and gathers them into one sorted run
// per series. One series may come in several batches:
//
//	batch 1    cpu{host="a"}   10       12
//	batch 2    cpu{host="a"}       11   12'
//	prepared   cpu{host="a"}   10  11   12'     12' was supplied last
func (s *Store) prepareIngest(batches []Batch, accepted window) ([]preparedBatch, error) {
	input := ingestInput{
		accepted:   accepted,
		maxSamples: s.opts.MaxBatchSamples,
		maxBytes:   s.opts.MaxBatchBytes,
		series:     map[string]*pendingSeries{},
	}
	for _, batch := range batches {
		if err := input.add(batch); err != nil {
			return nil, err
		}
	}
	return input.sorted(), nil
}

// window is the times one call accepts, read once from the store's clock:
//
//	now 12:00, Retention 30 days, ClockSkew 10 minutes → [12:00 thirty days ago, 12:10]
//
// A series kept by a rule of RetentionOf starts its window at its own keep.
type window struct {
	now, horizon int64
	retention    retention
}

func (s *Store) window() window {
	return s.windowAt(s.now().UnixMilli())
}

func (s *Store) windowAt(now int64) window {
	return window{now: now, horizon: later(now, s.opts.ClockSkew.Milliseconds()), retention: s.retention}
}

// ingestInput is one call being checked against its sample and byte budgets.
type ingestInput struct {
	accepted             window
	maxSamples, maxBytes int
	samples, bytes       int
	series               map[string]*pendingSeries
}

func (in *ingestInput) add(batch Batch) error {
	if len(batch.Samples) == 0 {
		return fmt.Errorf("%w: empty series batch", ErrInvalid)
	}
	if len(batch.Samples) > in.maxSamples-in.samples {
		return fmt.Errorf("%w: batch samples", ErrLimit)
	}
	in.samples += len(batch.Samples)

	kept, err := keptLabels(batch.Series.Name, batch.Series.Labels)
	if err != nil {
		return refusedSeries(batch.Series, err)
	}
	labels, identity, err := canonicalLabels(kept, true)
	if err != nil {
		return refusedSeries(batch.Series, err)
	}
	kind, err := seriesKind(batch.Series.Kind)
	if err != nil {
		return seriesError(labels, err)
	}

	// the canonical labels once, then a timestamp and a value per sample
	cost := len(identity) + 16*len(batch.Samples)
	if cost > in.maxBytes-in.bytes {
		return fmt.Errorf("%w: batch bytes", ErrLimit)
	}
	in.bytes += cost

	series, err := in.seriesFor(identity, labels, kind)
	if err != nil {
		return seriesError(labels, err)
	}
	if err = series.addAll(batch.Samples, in.accepted.horizon); err != nil {
		return seriesError(labels, err)
	}
	return nil
}

func (in *ingestInput) seriesFor(identity string, labels []label, kind Kind) (*pendingSeries, error) {
	series, found := in.series[identity]
	if !found {
		cutoff := in.accepted.retention.cutoff(in.accepted.now, seriesName(labels))
		series = &pendingSeries{batch: preparedBatch{identity: identity, labels: labels, kind: kind, cutoff: cutoff}}
		in.series[identity] = series
	}
	if series.batch.kind != kind {
		return nil, fmt.Errorf("%w: conflicting kinds in batch", ErrInvalid)
	}
	return series, nil
}

// sorted returns the series in identity order, so a call registers new series
// in the same order every time it is made.
func (in *ingestInput) sorted() []preparedBatch {
	out := make([]preparedBatch, 0, len(in.series))
	for _, series := range in.series {
		out = append(out, series.sorted())
	}
	sort.Slice(out, func(i, j int) bool { return out[i].identity < out[j].identity })
	return out
}

func seriesKind(kind Kind) (Kind, error) {
	switch kind {
	case "":
		return Gauge, nil
	case Gauge, Counter:
		return kind, nil
	}
	return "", fmt.Errorf("%w: series kind", ErrInvalid)
}

// pendingSeries keeps ordered input as a plain slice. The first repeated or
// out-of-order timestamp moves everything into a map keyed by time.
type pendingSeries struct {
	batch  preparedBatch
	byTime map[int64]Sample // nil while every timestamp so far was newer than the last
}

func (p *pendingSeries) addAll(samples []Sample, horizon int64) error {
	for _, point := range samples {
		switch {
		case point.At == math.MaxInt64:
			return fmt.Errorf("%w: MaxInt64 is reserved for the exclusive range bound", ErrInvalid)
		case point.At < p.batch.cutoff:
			return fmt.Errorf("%w: retention cutoff", ErrTooOld)
		case point.At > horizon:
			return fmt.Errorf("%w: %d is past %d, the clock and its skew", ErrTooNew, point.At, horizon)
		}
		p.add(point)
	}
	return nil
}

func (p *pendingSeries) add(point Sample) {
	if p.byTime == nil && !p.extends(point.At) {
		p.byTime = make(map[int64]Sample, len(p.batch.samples)+1)
		for _, earlier := range p.batch.samples {
			p.byTime[earlier.At] = earlier
		}
		p.batch.samples = nil
	}

	if p.byTime != nil {
		p.byTime[point.At] = point
		return
	}
	p.batch.samples = append(p.batch.samples, point)
}

func (p *pendingSeries) extends(at int64) bool {
	samples := p.batch.samples
	return len(samples) == 0 || at > samples[len(samples)-1].At
}

func (p *pendingSeries) sorted() preparedBatch {
	if p.byTime == nil {
		return p.batch
	}
	for _, point := range p.byTime {
		p.batch.samples = append(p.batch.samples, point)
	}
	samples := p.batch.samples
	sort.Slice(samples, func(i, j int) bool { return samples[i].At < samples[j].At })
	return p.batch
}

func countSamples(input []preparedBatch) int {
	total := 0
	for _, series := range input {
		total += len(series.samples)
	}
	return total
}
