package metrics

import (
	"context"
	"errors"
	"fmt"
	"math"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
)

// CounterInstrument only grows. Its total since the process started is ingested as a
// counter sample at each flush, so a restart is a reset Aggregate counts.
type CounterInstrument struct{ *instrument }

// GaugeInstrument is a value set or moved by the application, ingested at each flush.
type GaugeInstrument struct{ *instrument }

func (c CounterInstrument) Inc() { c.Add(1) }

// Add grows the counter by n; a negative or non-finite n is dropped and
// logged once, since a counter that shrinks would read as a reset.
func (c CounterInstrument) Add(n float64) {
	if n < 0 || math.IsNaN(n) || math.IsInf(n, 0) {
		c.warnOnce(fmt.Errorf("%w: counter increment %v", ErrInvalid, n))
		return
	}
	for {
		old := c.value.Load()
		next := math.Float64frombits(old) + n
		if math.IsInf(next, 0) {
			c.warnOnce(fmt.Errorf("%w: counter total overflow", ErrInvalid))
			return
		}
		if c.value.CompareAndSwap(old, math.Float64bits(next)) {
			return
		}
	}
}

// With is the counter of the same name with more labels, given as name, value
// pairs; the same labels in any order are the same series.
func (c CounterInstrument) With(labels ...string) CounterInstrument {
	return CounterInstrument{c.child(labels)}
}

func (g GaugeInstrument) Set(value float64) { g.value.Store(math.Float64bits(value)) }
func (g GaugeInstrument) Add(delta float64) { g.add(delta) }

func (g GaugeInstrument) With(labels ...string) GaugeInstrument {
	return GaugeInstrument{g.child(labels)}
}

func (s *Store) Counter(name string) CounterInstrument {
	return CounterInstrument{s.instruments.register(named(Counter, name))}
}

func (s *Store) Gauge(name string) GaugeInstrument {
	return GaugeInstrument{s.instruments.register(named(Gauge, name))}
}

// GaugeFunc asks read for the gauge's value at each flush; an error skips that
// sample and is logged, once while it stays the same.
func (s *Store) GaugeFunc(name string, read func(context.Context) (float64, error)) {
	gauge := s.instruments.register(named(Gauge, name))
	if read == nil {
		gauge.warnOnce(fmt.Errorf("%w: nil gauge callback", ErrInvalid))
		return
	}
	gauge.read.Store(&gaugeReader{call: read})
}

func named(kind Kind, name string) Series {
	return Series{Kind: kind, Labels: []Label{{Name: "__name__", Value: name}}}
}

type instrument struct {
	set    *instruments
	series Series
	value  atomic.Uint64 // float64 bits
	read   atomic.Pointer[gaugeReader]

	warned   atomic.Bool
	refused  atomic.Bool
	lastRead string
}

type gaugeReader struct {
	call func(context.Context) (float64, error)
}

func (i *instrument) add(delta float64) {
	for {
		old := i.value.Load()
		if i.value.CompareAndSwap(old, math.Float64bits(math.Float64frombits(old)+delta)) {
			return
		}
	}
}

func (i *instrument) child(pairs []string) *instrument {
	labels := slices.Clone(i.series.Labels)
	for index := 0; index+1 < len(pairs); index += 2 {
		labels = append(labels, Label{Name: pairs[index], Value: pairs[index+1]})
	}
	if len(pairs)%2 != 0 {
		child := &instrument{set: i.set, series: Series{Kind: i.series.Kind, Labels: labels}}
		child.refused.Store(true)
		child.warnOnce(fmt.Errorf("%w: label %q has no value", ErrInvalid, pairs[len(pairs)-1]))
		return child
	}
	return i.set.register(Series{Kind: i.series.Kind, Labels: labels})
}

func (i *instrument) warnOnce(err error) {
	if i.warned.CompareAndSwap(false, true) {
		i.set.store.log.Warn("instrument refused", "series", formatLabels(i.series.Labels), "error", err)
	}
}

// instruments is every counter and gauge instrument of one engine, by label set
type instruments struct {
	store     *Store
	mu        sync.Mutex
	all       map[string]*instrument
	conflicts map[conflictKey]*instrument
}

type conflictKey struct {
	labels string
	kind   Kind
}

func (set *instruments) register(series Series) *instrument {
	key := labelKey(series.Labels)
	set.mu.Lock()
	if existing, ok := set.all[key]; ok {
		if existing.series.Kind == series.Kind {
			set.mu.Unlock()
			return existing
		}
		conflict := conflictKey{labels: key, kind: series.Kind}
		refused := set.conflicts[conflict]
		if refused == nil {
			refused = &instrument{set: set, series: series}
			refused.refused.Store(true)
			if set.conflicts == nil {
				set.conflicts = make(map[conflictKey]*instrument)
			}
			set.conflicts[conflict] = refused
		}
		set.mu.Unlock()
		refused.warnOnce(fmt.Errorf("%w: registered as both counter and gauge", ErrConflict))
		return refused
	}
	if set.all == nil {
		set.all = map[string]*instrument{}
	}
	created := &instrument{set: set, series: series}
	set.all[key] = created
	set.mu.Unlock()
	return created
}

func (set *instruments) snapshot() []*instrument {
	set.mu.Lock()
	defer set.mu.Unlock()
	out := make([]*instrument, 0, len(set.all))
	for _, instrument := range set.all {
		if !instrument.refused.Load() {
			out = append(out, instrument)
		}
	}
	return out
}

// labelKey is one series whatever order its labels were given in
func labelKey(labels []Label) string {
	pairs := make([]string, len(labels))
	for i, label := range labels {
		pairs[i] = label.Name + "\xff" + label.Value
	}
	slices.Sort(pairs)
	return strings.Join(pairs, "\xfe")
}

// Flush ingests every instrument's value now. A series Ingest refuses, an
// invalid label say, is dropped and logged once rather than keeping the rest
// out: Ingest is atomic, so the flush is retried without it.
func (s *Store) Flush(ctx context.Context) error {
	s.flushing.Lock()
	defer s.flushing.Unlock()

	batches := s.instrumentBatches(ctx, s.now().UnixMilli())
	for len(batches) > 0 {
		err := s.Ingest(ctx, batches)
		var refused *SeriesError
		if !errors.As(err, &refused) {
			return err
		}
		remaining := s.dropRefused(batches, refused)
		if len(remaining) == len(batches) {
			return err
		}
		batches = remaining
	}
	return nil
}

func (s *Store) instrumentBatches(ctx context.Context, at int64) []Batch {
	var batches []Batch
	for _, instrument := range s.instruments.snapshot() {
		value, ok := instrument.current(ctx)
		if ok {
			batches = append(batches, Batch{Series: instrument.series, Samples: []Sample{{At: at, Value: value}}})
		}
	}
	return batches
}

func (i *instrument) current(ctx context.Context) (float64, bool) {
	reader := i.read.Load()
	if reader == nil {
		return math.Float64frombits(i.value.Load()), true
	}
	value, err := reader.call(ctx)
	if err != nil {
		if err.Error() != i.lastRead {
			i.set.store.log.Warn("gauge read failed", "series", formatLabels(i.series.Labels), "error", err)
			i.lastRead = err.Error()
		}
		return 0, false
	}
	i.lastRead = ""
	return value, true
}

func (s *Store) dropRefused(batches []Batch, refused *SeriesError) []Batch {
	key := labelKey(refused.Labels)
	s.instruments.mu.Lock()
	instrument := s.instruments.all[key]
	s.instruments.mu.Unlock()
	if instrument != nil {
		instrument.refused.Store(true)
		instrument.warnOnce(refused.Err)
	}
	return slices.DeleteFunc(batches, func(batch Batch) bool { return labelKey(batch.Series.Labels) == key })
}
