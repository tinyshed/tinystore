package metrics

import (
	"context"
	"fmt"
	"math"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
)

// A program's instruments: counters, gauges, a gauge read from a function,
// timers and histograms, each set of labels a series of its own. A call
// changes a number here and never waits, never fails: what it refuses, a
// value or a label, changes nothing and is said once in TinyStore's own
// lines, as the metrics crate's and Prometheus' client's calls do. What
// changed goes to the store's totals on a stream of its own, a batch a
// second; see send.go.

const (
	// mostSets is the sets of labels an instrument holds; past them it counts into the series of __overflow.
	mostSets = 2000
	// idle is how long a counter's or a timer's set of labels stays without a call.
	idle       = time.Hour
	mostBounds = 128
)

type form string

const (
	formCounter   form = "counter"
	formGauge     form = "gauge"
	formTimer     form = "timer"
	formHistogram form = "histogram"
)

// Option is what an instrument, or IngestPrometheus, is told.
type Option interface{ instrument(*told) }

type told struct {
	unit, help  *string
	keep        *uint64
	bounds      []float64
	exponential bool
	labels      Labels
	at          *time.Time
	refused     error
}

type optionFunc func(*told)

func (f optionFunc) instrument(t *told) { f(t) }

// Help is a line on what the name means, 1024 bytes at most.
func Help(text string) Option { return optionFunc(func(t *told) { t.help = &text }) }

// Unit is the unit of its values, 32 bytes at most: "bytes", "%"; a timer's is ms.
func Unit(text string) Option { return optionFunc(func(t *told) { t.unit = &text }) }

// Keep is how long the name's samples stay, kept in the file with it: 30 days
// unless a name was given one.
func Keep(d time.Duration) Option {
	return optionFunc(func(t *told) {
		ms, err := core.Millis(d, "keep")
		if err == nil && ms == 0 {
			err = core.Errorf(core.Invalid, "keep 0, where a name keeps its samples 1ms at least")
		}
		t.keep, t.refused = &ms, err
	})
}

// Buckets are a timer's or a histogram's upper bounds, ascending; the store
// adds +Inf, and a percentile of it becomes possible.
func Buckets(bounds ...float64) Option {
	return optionFunc(func(t *told) { t.bounds = append([]float64{}, bounds...) })
}

// Exponential lets the store keep a timer's spread on Prometheus' native
// grid, eight intervals a doubling, an experiment: a percentile then lies
// within one interval, with no bound to choose.
func Exponential() Option { return optionFunc(func(t *told) { t.exponential = true }) }

// At is the time of every sample of IngestPrometheus' text that gives none.
func At(at time.Time) Option { return optionFunc(func(t *told) { t.at = &at }) }

// instrument gives a gauge function the set of labels it reads, and
// IngestPrometheus those every series gets beside its own.
func (l Labels) instrument(t *told) { t.labels = l }

// ExponentialBuckets is upper bounds from start, each factor times the one
// before, as Prometheus' ExponentialBuckets: ExponentialBuckets(1, 2, 14) is 1, 2, 4 … 8192.
func ExponentialBuckets(start, factor float64, count int) []float64 {
	if !(start > 0 && factor > 1 && count >= 1 && count <= mostBounds) {
		return nil
	}
	bounds := []float64{start}
	for len(bounds) < count {
		bounds = append(bounds, bounds[len(bounds)-1]*factor)
	}
	return bounds
}

func toldOf(name string, f form, opts []Option) (told, error) {
	var t told
	for _, opt := range opts {
		opt.instrument(&t)
	}
	switch {
	case t.refused != nil:
		return t, core.Wrap(core.Invalid, t.refused, "metrics %s", name)
	case t.at != nil:
		return t, core.Errorf(core.Invalid, "metrics %s: At is IngestPrometheus' alone", name)
	case name == "" || len(name) > 4096:
		return t, core.Errorf(core.Invalid, "metrics name %q: 1 to 4096 bytes of UTF-8", name)
	case t.unit != nil && len(*t.unit) > 32:
		return t, core.Errorf(core.Invalid, "metrics %s: a unit of 32 bytes at most", name)
	case t.help != nil && len(*t.help) > 1024:
		return t, core.Errorf(core.Invalid, "metrics %s: a help of 1024 bytes at most", name)
	case (t.bounds != nil || t.exponential) && f != formTimer && f != formHistogram:
		return t, core.Errorf(core.Invalid, "metrics %s: buckets are a timer's or a histogram's", name)
	case t.bounds != nil && t.exponential:
		return t, core.Errorf(core.Invalid, "metrics %s: buckets, or Exponential, not both", name)
	}
	if t.bounds != nil {
		if len(t.bounds) < 1 || len(t.bounds) > mostBounds {
			return t, core.Errorf(core.Invalid, "metrics %s: 1 to %d bounds", name, mostBounds)
		}
		for i, bound := range t.bounds {
			if math.IsNaN(bound) || math.IsInf(bound, 0) || bound < 0 || i > 0 && bound <= t.bounds[i-1] {
				return t, core.Errorf(core.Invalid, "metrics %s: bounds of 0 or more, each above the one before", name)
			}
		}
	}
	return t, nil
}

// labelsWith is labels and more given in pairs, as slog's attributes go: a
// name and a value, a number or a bool kept as its text.
func labelsWith(labels Labels, timed bool, pairs []any) (Labels, error) {
	if len(pairs)%2 != 0 {
		return nil, core.Errorf(core.Invalid, "labels are given in pairs, a name and a value: %d given", len(pairs))
	}
	out := make(Labels, len(labels)+len(pairs)/2)
	for name, value := range labels {
		out[name] = value
	}
	for i := 0; i < len(pairs); i += 2 {
		name, ok := pairs[i].(string)
		switch {
		case !ok || name == "" || len(name) > 256:
			return nil, core.Errorf(core.Invalid, "label %v: a name of 1 to 256 bytes", pairs[i])
		case strings.HasPrefix(name, "__"):
			return nil, core.Errorf(core.Invalid, "label %s: a name beginning with __ is the store's own", name)
		case timed && name == "le":
			return nil, core.Errorf(core.Invalid, "label le: a timer's _bucket series give it")
		}
		text, err := labelText(pairs[i+1])
		if err != nil {
			return nil, core.Wrap(core.Invalid, err, "label %s", name)
		}
		if len(text) > 4096 {
			return nil, core.Errorf(core.Invalid, "label %s: a value of 4096 bytes at most", name)
		}
		out[name] = text
	}
	return out, nil
}

// labelText is a label's value as the store keeps it: a number or a bool as
// its text, 200 as "200", true as "true".
func labelText(value any) (string, error) {
	switch v := value.(type) {
	case string:
		return v, nil
	case bool:
		return strconv.FormatBool(v), nil
	case int:
		return strconv.Itoa(v), nil
	case int8, int16, int32, int64:
		return fmt.Sprint(v), nil
	case uint, uint8, uint16, uint32, uint64:
		return fmt.Sprint(v), nil
	case float32:
		return numberText(float64(v)), nil
	case float64:
		return numberText(v), nil
	case fmt.Stringer:
		return v.String(), nil
	}
	return "", core.Errorf(core.Invalid, "a value of text, a number or a bool, where %v is", value)
}

// numberText is a number as JavaScript's String writes it, the fewest digits
// that read back as it, so that every language writes one label and one
// bound: 5, 0.25, 1e+21, 1e-7.
func numberText(v float64) string {
	switch abs := math.Abs(v); {
	case v == 0:
		return "0"
	case math.IsNaN(v):
		return "NaN"
	case math.IsInf(v, 1):
		return "Infinity"
	case math.IsInf(v, -1):
		return "-Infinity"
	case abs >= 1e-6 && abs < 1e21:
		return strconv.FormatFloat(v, 'f', -1, 64)
	}
	mantissa, exponent, _ := strings.Cut(strconv.FormatFloat(v, 'e', -1, 64), "e")
	digits := strings.TrimLeft(exponent[1:], "0")
	return mantissa + "e" + exponent[:1] + digits
}

// fractions are Prometheus' native grid for schema 3, 2^(j/8 − 1), as its
// table writes them: three a unit in the last place below the nearest float,
// so that a value on a bound falls where Prometheus and the store put it.
var fractions = [8]float64{
	0.5, 0.5452538663326288, 0.5946035575013605, 0.6484197773255048,
	0.7071067811865475, 0.7711054127039704, 0.8408964152537144, 0.9170040432046711,
}

// binOf is the interval of the store's grid a value of 0 or more falls in, by its label: "3:13", or "zero".
func binOf(value float64) string {
	if value == 0 {
		return "zero"
	}
	// a subnormal falls in the lowest interval, whose bound is the least normal float
	bits := math.Float64bits(max(value, 2.2250738585072014e-308))
	exponent := int((bits>>52)&0x7ff) - 1022
	fraction := math.Float64frombits(bits&^(0x7ff<<52) | 1022<<52)
	within := 8
	for i, bound := range fractions {
		if bound >= fraction {
			within = i
			break
		}
	}
	return "3:" + strconv.Itoa(within+(exponent-1)*8)
}

// instrumentsOf is the store's instruments, made the first time.
func instrumentsOf(store *tinystore.Store) *instruments {
	held := store.CoreHome().Store
	return held.Kept("metrics.instruments", func() any { return newInstruments(held) }).(*instruments)
}

// refused is a call an instrument refused, said once a process for each
// instrument and why, since a call returns nothing.
var refused sync.Map

func refuse(name string, err error) {
	if _, said := refused.LoadOrStore(name+"\x00"+err.Error(), true); !said {
		core.SayOwn("WARN", "metrics call refused", "instrument", name, "error", err)
	}
}

// OpenCounter is a total that only goes up, which the store keeps:
// processes counting into one series count into one total. Another form or
// other options than the name has while the store is open is Invalid.
func OpenCounter(store *tinystore.Store, name string, opts ...Option) (*Counter, error) {
	d, err := instrumentsOf(store).declare(name, formCounter, opts, false)
	if err != nil {
		return nil, err
	}
	return &Counter{in: d.in, d: d}, nil
}

// OpenGauge is a value that goes up and down, written while this program is
// connected: a queue's length, a temperature.
func OpenGauge(store *tinystore.Store, name string, opts ...Option) (*Gauge, error) {
	d, err := instrumentsOf(store).declare(name, formGauge, opts, false)
	if err != nil {
		return nil, err
	}
	return &Gauge{in: d.in, d: d}, nil
}

// OpenGaugeFunc is a gauge whose value read gives every 15 seconds and at
// Flush, of the set of labels Labels gives, none unless given. Another set of
// the name takes a function of its own. It is never called again while a
// call of it runs, and an error or a value that is no finite number writes
// nothing that time, said once while it repeats.
//
//	err = metrics.OpenGaugeFunc(store, "queue_depth", emails.Depth, metrics.Labels{"queue": "emails"})
func OpenGaugeFunc(store *tinystore.Store, name string, read func(ctx context.Context) (float64, error), opts ...Option) error {
	in := instrumentsOf(store)
	d, err := in.declare(name, formGauge, opts, true)
	if err != nil {
		return err
	}
	var t told
	for _, opt := range opts {
		opt.instrument(&t)
	}
	labels, err := labelsWith(t.labels, false, nil)
	if err != nil {
		return core.Wrap(core.Invalid, err, "metrics %s", name)
	}
	in.mu.Lock()
	defer in.mu.Unlock()
	key := keyOf(labels)
	if _, held := d.reads[key]; held {
		return core.Errorf(core.Invalid, "metrics %s: a function reads its gauge of %v already while the store is open", name, labels)
	}
	d.reads[key] = &gaugeRead{labels: labels, read: read}
	return nil
}

// OpenTimer is how many things took how long: their count, sum and longest
// in milliseconds, and their spread with Buckets. A timer named
// http_request_ms writes http_request_ms_count, _sum, _max and, with
// buckets, _bucket.
func OpenTimer(store *tinystore.Store, name string, opts ...Option) (*Timer, error) {
	d, err := instrumentsOf(store).declare(name, formTimer, opts, false)
	if err != nil {
		return nil, err
	}
	return &Timer{in: d.in, d: d}, nil
}

// OpenHistogram is a timer of numbers that are not durations: a response's size in bytes.
func OpenHistogram(store *tinystore.Store, name string, opts ...Option) (*Histogram, error) {
	d, err := instrumentsOf(store).declare(name, formHistogram, opts, false)
	if err != nil {
		return nil, err
	}
	return &Histogram{in: d.in, d: d}, nil
}

// Counter is a total that only goes up.
type Counter struct {
	in      *instruments
	d       *declared
	labels  Labels
	refused error
}

// With is the same counter with more labels, given in pairs, a series of its
// own; the same labels in any order are one.
func (c *Counter) With(pairs ...any) *Counter {
	labels, err := labelsWith(c.labels, false, pairs)
	return &Counter{in: c.in, d: c.d, labels: labels, refused: err}
}

// Inc adds one.
func (c *Counter) Inc() { c.Add(1) }

// Add adds n, a finite number of 0 or more; Add(0) makes the set of labels
// known with a total of zero.
func (c *Counter) Add(n float64) {
	switch {
	case c.refused != nil:
		refuse(c.d.name, c.refused)
	case math.IsNaN(n) || math.IsInf(n, 0) || n < 0:
		refuse(c.d.name, core.Errorf(core.Invalid, "a counter adds %v; it only grows, by a finite number", n))
	default:
		c.in.add(c.d, c.labels, n)
	}
}

// Gauge is a value that goes up and down.
type Gauge struct {
	in      *instruments
	d       *declared
	labels  Labels
	refused error
}

// With is the same gauge with more labels, given in pairs, a series of its own.
func (g *Gauge) With(pairs ...any) *Gauge {
	labels, err := labelsWith(g.labels, false, pairs)
	return &Gauge{in: g.in, d: g.d, labels: labels, refused: err}
}

// Set sets its value, a finite number.
func (g *Gauge) Set(value float64) { g.move(value, false) }

// Add moves it by n, from zero when it was never set.
func (g *Gauge) Add(n float64) { g.move(n, true) }

func (g *Gauge) Inc() { g.Add(1) }
func (g *Gauge) Dec() { g.Add(-1) }

func (g *Gauge) move(n float64, by bool) {
	switch {
	case g.refused != nil:
		refuse(g.d.name, g.refused)
	case math.IsNaN(n) || math.IsInf(n, 0):
		refuse(g.d.name, core.Errorf(core.Invalid, "a gauge set to %v, where a finite number is", n))
	default:
		if err := g.in.set(g.d, g.labels, n, by); err != nil {
			refuse(g.d.name, err)
		}
	}
}

// Timer is how many things took how long.
type Timer struct {
	in      *instruments
	d       *declared
	labels  Labels
	refused error
}

// With is the same timer with more labels, given in pairs, a series of its own; le is its _bucket series'.
func (t *Timer) With(pairs ...any) *Timer {
	labels, err := labelsWith(t.labels, true, pairs)
	return &Timer{in: t.in, d: t.d, labels: labels, refused: err}
}

// Record records a duration measured elsewhere.
func (t *Timer) Record(d time.Duration) {
	switch {
	case t.refused != nil:
		refuse(t.d.name, t.refused)
	case d < 0:
		refuse(t.d.name, core.Errorf(core.Invalid, "a duration of %s; it is never negative", d))
	default:
		t.in.measure(t.d, t.labels, float64(d)/float64(time.Millisecond))
	}
}

// Start is a timing that records when it stops, for a span that is no one
// call: defer latency.Start().Stop().
func (t *Timer) Start() *Timing { return &Timing{timer: t, started: time.Now()} }

// Timing is a span a timer measures from its start.
type Timing struct {
	timer   *Timer
	started time.Time
	stopped atomic.Bool
}

// Stop records how long since the timing started, the first time it is
// called, and gives it.
func (t *Timing) Stop() time.Duration {
	took := time.Since(t.started)
	if !t.stopped.Swap(true) {
		t.timer.Record(took)
	}
	return took
}

// Histogram is how many numbers of what size: their count, sum and largest, and their spread.
type Histogram struct {
	in      *instruments
	d       *declared
	labels  Labels
	refused error
}

// With is the same histogram with more labels, given in pairs, a series of its own.
func (h *Histogram) With(pairs ...any) *Histogram {
	labels, err := labelsWith(h.labels, true, pairs)
	return &Histogram{in: h.in, d: h.d, labels: labels, refused: err}
}

// Record records a value of 0 or more.
func (h *Histogram) Record(value float64) {
	switch {
	case h.refused != nil:
		refuse(h.d.name, h.refused)
	case math.IsNaN(value) || math.IsInf(value, 0) || value < 0:
		refuse(h.d.name, core.Errorf(core.Invalid, "a value of %v; the lowest bucket starts at 0", value))
	default:
		h.in.measure(h.d, h.labels, value)
	}
}
