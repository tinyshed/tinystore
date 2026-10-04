package metrics

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"maps"
	"math"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

// CounterInstrument only grows. Its total since the process started is ingested
// as a counter sample at each flush, so a restart is a reset Aggregate counts.
type CounterInstrument struct{ *instrument }

// GaugeInstrument is a value set or moved by the application, ingested at each flush.
type GaugeInstrument struct{ *instrument }

// TimerInstrument measures how long something takes. Each flush ingests how
// many durations it measured and their sum in milliseconds, since the process
// started, as the counters <name>_count and <name>_sum, and the longest since
// the flush before as the gauge <name>_max, left out when it measured none.
// A range's mean is the increase of its sum over the increase of its count.
// Given a description, its sum and longest are in milliseconds, whatever Unit
// it is given.
type TimerInstrument struct{ *timer }

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

// Record adds one duration; a negative one is dropped and logged once.
func (t TimerInstrument) Record(d time.Duration) {
	if d < 0 {
		t.warnOnce(fmt.Errorf("%w: timer duration %v", ErrInvalid, d))
		return
	}
	t.add(float64(d) / float64(time.Millisecond))
}

// Since records the time from start to now, so that a deferred call times the
// function it is deferred in: defer latency.Since(time.Now()).
func (t TimerInstrument) Since(start time.Time) { t.Record(time.Since(start)) }

// With is the timer of the same name with more labels, given as name, value
// pairs; the same labels in any order are the same series.
func (t TimerInstrument) With(labels ...string) TimerInstrument {
	return TimerInstrument{t.child(labels)}
}

// Counter is the counter of a name; a description given to it is written at
// the next flush, as Describe would.
func (s *Store) Counter(name string, options ...DescribeOption) CounterInstrument {
	s.instruments.describe(name, describedBy(options))
	return CounterInstrument{s.instruments.register(named(Counter, name))}
}

func (s *Store) Gauge(name string, options ...DescribeOption) GaugeInstrument {
	s.instruments.describe(name, describedBy(options))
	return GaugeInstrument{s.instruments.register(named(Gauge, name))}
}

func (s *Store) Timer(name string, options ...DescribeOption) TimerInstrument {
	if len(options) > 0 {
		help := describedBy(options).Help
		s.instruments.describe(name+"_count", Description{Help: help})
		s.instruments.describe(name+"_sum", Description{Unit: "ms", Help: help})
		s.instruments.describe(name+"_max", Description{Unit: "ms", Help: help})
	}
	return TimerInstrument{s.instruments.registerTimer(Series{Name: name})}
}

// GaugeFunc asks read for the gauge's value at each flush; an error skips that
// sample and is logged, once while it stays the same.
func (s *Store) GaugeFunc(name string, read func(context.Context) (float64, error), options ...DescribeOption) {
	s.instruments.describe(name, describedBy(options))
	gauge := s.instruments.register(named(Gauge, name))
	if read == nil {
		gauge.warnOnce(fmt.Errorf("%w: nil gauge callback", ErrInvalid))
		return
	}
	gauge.read.Store(&gaugeReader{call: read})
}

func named(kind Kind, name string) Series {
	return Series{Name: name, Kind: kind}
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
	labels, err := childLabels(i.series.Labels, pairs)
	series := Series{Name: i.series.Name, Kind: i.series.Kind, Labels: labels}
	if err != nil {
		child := &instrument{set: i.set, series: series}
		child.refused.Store(true)
		child.warnOnce(err)
		return child
	}
	return i.set.register(series)
}

// childLabels is parent's labels with the name, value pairs given after them
func childLabels(parent Labels, pairs []string) (Labels, error) {
	labels := make(Labels, len(parent)+len(pairs)/2)
	maps.Copy(labels, parent)
	for index := 0; index+1 < len(pairs); index += 2 {
		labels[pairs[index]] = pairs[index+1]
	}
	if len(pairs)%2 != 0 {
		return labels, fmt.Errorf("%w: label %q has no value", ErrInvalid, pairs[len(pairs)-1])
	}
	return labels, nil
}

func (i *instrument) warnOnce(err error) {
	if i.warned.CompareAndSwap(false, true) {
		i.set.store.log.Warn("instrument refused", "series", i.series.String(), "error", err)
	}
}

// timer is a timer's durations since the process started and the longest
// since the flush before, under one lock so that a flush takes the three
// together: a count without the sum it went with would skew a range's mean.
type timer struct {
	set    *instruments
	series Series // its name and labels, without a kind: what it ingests is written

	mu       sync.Mutex
	count    uint64
	sum      float64 // milliseconds
	longest  float64 // milliseconds, since the flush before
	measured bool    // since the flush before

	warned  atomic.Bool
	refused atomic.Bool
}

func (t *timer) add(ms float64) {
	t.mu.Lock()
	t.count++
	t.sum += ms
	t.longest = max(t.longest, ms)
	t.measured = true
	t.mu.Unlock()
}

func (t *timer) child(pairs []string) *timer {
	labels, err := childLabels(t.series.Labels, pairs)
	series := Series{Name: t.series.Name, Labels: labels}
	if err != nil {
		child := &timer{set: t.set, series: series}
		child.refused.Store(true)
		child.warnOnce(err)
		return child
	}
	return t.set.registerTimer(series)
}

// written is the series a timer ingests: its count and sum, and its longest
func (t *timer) written() [3]Series {
	return [3]Series{
		{Name: t.series.Name + "_count", Kind: Counter, Labels: t.series.Labels},
		{Name: t.series.Name + "_sum", Kind: Counter, Labels: t.series.Labels},
		{Name: t.series.Name + "_max", Kind: Gauge, Labels: t.series.Labels},
	}
}

func (t *timer) writtenKeys() []string {
	written := t.written()
	return []string{seriesKey(written[0]), seriesKey(written[1]), seriesKey(written[2])}
}

// take is what a flush at the time at ingests of the timer, and starts its
// longest again: the longest is left out when nothing was measured since
func (t *timer) take(at int64) (batches []Batch, longest float64, measured bool) {
	t.mu.Lock()
	count, sum := t.count, t.sum
	longest, measured = t.longest, t.measured
	t.longest, t.measured = 0, false
	t.mu.Unlock()

	written := t.written()
	batches = []Batch{
		{Series: written[0], Samples: []Sample{{At: at, Value: float64(count)}}},
		{Series: written[1], Samples: []Sample{{At: at, Value: sum}}},
	}
	if measured {
		batches = append(batches, Batch{Series: written[2], Samples: []Sample{{At: at, Value: longest}}})
	}
	return batches, longest, measured
}

// giveBack keeps the longest of a flush that failed for the next one
func (t *timer) giveBack(longest float64) {
	t.mu.Lock()
	t.longest = max(t.longest, longest)
	t.measured = true
	t.mu.Unlock()
}

func (t *timer) warnOnce(err error) {
	if t.warned.CompareAndSwap(false, true) {
		t.set.store.log.Warn("instrument refused", "series", t.series.String(), "error", err)
	}
}

// instruments is every counter, gauge and timer of one engine, by label set.
// A series has one writer: a counter, a gauge or one of a timer's three.
type instruments struct {
	store     *Store
	mu        sync.Mutex
	all       map[string]*instrument
	conflicts map[conflictKey]*instrument
	timers    map[string]*timer      // by their own name and labels
	timed     map[string]*timer      // by each series they write
	described map[string]Description // by name, for the next flush to write
}

// describe keeps a name's description for the next flush; one past its
// bounds is logged and left out, since an instrument returns no error.
func (set *instruments) describe(name string, description Description) {
	if description == (Description{}) {
		return
	}
	if err := checkDescription(name, description); err != nil {
		set.store.log.Warn("description refused", "name", name, "error", err)
		return
	}
	set.mu.Lock()
	defer set.mu.Unlock()
	if set.described == nil {
		set.described = map[string]Description{}
	}
	set.described[name] = description
}

// takeDescriptions is what the next flush writes; the flush gives them back
// when it fails.
func (set *instruments) takeDescriptions() map[string]Description {
	set.mu.Lock()
	defer set.mu.Unlock()
	taken := set.described
	set.described = nil
	return taken
}

func (set *instruments) giveBackDescriptions(taken map[string]Description) {
	set.mu.Lock()
	defer set.mu.Unlock()
	for name, description := range taken {
		if _, newer := set.described[name]; !newer {
			if set.described == nil {
				set.described = map[string]Description{}
			}
			set.described[name] = description
		}
	}
}

type conflictKey struct {
	labels string
	kind   Kind
}

func (set *instruments) register(series Series) *instrument {
	key := seriesKey(series)
	set.mu.Lock()
	existing, registered := set.all[key]
	if registered && existing.series.Kind == series.Kind {
		set.mu.Unlock()
		return existing
	}
	var conflict error
	if registered {
		conflict = fmt.Errorf("%w: registered as both counter and gauge", ErrConflict)
	} else if owner := set.timed[key]; owner != nil {
		conflict = fmt.Errorf("%w: written by the timer %s", ErrConflict, owner.series)
	}
	if conflict != nil {
		refused := set.refusedFor(key, series)
		set.mu.Unlock()
		refused.warnOnce(conflict)
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

// refusedFor is the one refused instrument of a conflicting registration, so
// that asking for it again logs nothing more
func (set *instruments) refusedFor(key string, series Series) *instrument {
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
	return refused
}

// registerTimer refuses a timer one of whose series a counter or a gauge
// writes already; it keeps the refused timer, so that asking again logs once.
func (set *instruments) registerTimer(series Series) *timer {
	key := seriesKey(series)
	set.mu.Lock()
	if existing, ok := set.timers[key]; ok {
		set.mu.Unlock()
		return existing
	}
	created := &timer{set: set, series: series}
	var writer *instrument
	for _, written := range created.writtenKeys() {
		writer = cmp.Or(writer, set.all[written])
	}
	if set.timers == nil {
		set.timers, set.timed = map[string]*timer{}, map[string]*timer{}
	}
	set.timers[key] = created
	if writer == nil {
		for _, written := range created.writtenKeys() {
			set.timed[written] = created
		}
	}
	set.mu.Unlock()
	if writer != nil {
		created.refused.Store(true)
		created.warnOnce(fmt.Errorf("%w: %s is a %s already", ErrConflict, writer.series, writer.series.Kind))
	}
	return created
}

func (set *instruments) snapshot() ([]*instrument, []*timer) {
	set.mu.Lock()
	defer set.mu.Unlock()
	instruments := make([]*instrument, 0, len(set.all))
	for _, instrument := range set.all {
		if !instrument.refused.Load() {
			instruments = append(instruments, instrument)
		}
	}
	timers := make([]*timer, 0, len(set.timers))
	for _, timer := range set.timers {
		if !timer.refused.Load() {
			timers = append(timers, timer)
		}
	}
	return instruments, timers
}

// refuse leaves the writer of series out of every flush, logging why once, and
// answers the keys of what that writer ingests: a timer's three series go together.
func (set *instruments) refuse(series Series, err error) []string {
	key := seriesKey(series)
	set.mu.Lock()
	instrument, timer := set.all[key], set.timed[key]
	set.mu.Unlock()
	switch {
	case instrument != nil:
		instrument.refused.Store(true)
		instrument.warnOnce(err)
	case timer != nil:
		timer.refused.Store(true)
		timer.warnOnce(err)
		return timer.writtenKeys()
	}
	return []string{key}
}

// seriesKey is one series whatever order its labels were given in
func seriesKey(series Series) string {
	pairs := make([]string, 0, len(series.Labels)+1)
	pairs = append(pairs, metricName+"\xff"+series.Name)
	for name, value := range series.Labels {
		pairs = append(pairs, name+"\xff"+value)
	}
	slices.Sort(pairs)
	return strings.Join(pairs, "\xfe")
}

// Flush ingests every instrument's value now. A series Ingest refuses, an
// invalid label say, is dropped and logged once rather than keeping the rest
// out: Ingest is atomic, so the flush is retried without it. A flush that
// fails keeps the longest durations it took for the next.
func (s *Store) Flush(ctx context.Context) error {
	s.flushing.Lock()
	defer s.flushing.Unlock()

	if err := s.writeDescriptions(ctx); err != nil {
		return err
	}
	batches, taken := s.instrumentBatches(ctx, s.now().UnixMilli())
	err := s.ingestInstruments(ctx, batches)
	if err != nil {
		for _, took := range taken {
			took.timer.giveBack(took.longest)
		}
	}
	return err
}

// longestTaken is a timer's longest duration a flush took from it
type longestTaken struct {
	timer   *timer
	longest float64
}

func (s *Store) instrumentBatches(ctx context.Context, at int64) ([]Batch, []longestTaken) {
	instruments, timers := s.instruments.snapshot()
	var batches []Batch
	for _, instrument := range instruments {
		value, ok := instrument.current(ctx)
		if ok {
			batches = append(batches, Batch{Series: instrument.series, Samples: []Sample{{At: at, Value: value}}})
		}
	}
	var taken []longestTaken
	for _, timer := range timers {
		timed, longest, measured := timer.take(at)
		batches = append(batches, timed...)
		if measured {
			taken = append(taken, longestTaken{timer: timer, longest: longest})
		}
	}
	return batches, taken
}

func (s *Store) ingestInstruments(ctx context.Context, batches []Batch) error {
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

func (i *instrument) current(ctx context.Context) (float64, bool) {
	reader := i.read.Load()
	if reader == nil {
		return math.Float64frombits(i.value.Load()), true
	}
	value, err := reader.call(ctx)
	if err != nil {
		if err.Error() != i.lastRead {
			i.set.store.log.Warn("gauge read failed", "series", i.series.String(), "error", err)
			i.lastRead = err.Error()
		}
		return 0, false
	}
	i.lastRead = ""
	return value, true
}

func (s *Store) dropRefused(batches []Batch, refused *SeriesError) []Batch {
	dropped := s.instruments.refuse(Series{Name: refused.Name, Labels: refused.Labels}, refused.Err)
	return slices.DeleteFunc(batches, func(batch Batch) bool {
		return slices.Contains(dropped, seriesKey(batch.Series))
	})
}
