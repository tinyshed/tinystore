# Metrics API for Go

Every public type, function and constant of the Metrics engine in `tinyshed.org/tinystore/metrics`, generated from its source. The [Metrics guide](../../guides/metrics/README.md) explains how to use them, and the [Bun and Node](../bun/metrics.md), [Python](../python/metrics.md) and [Rust](../rust/metrics.md) pages list the same API.

## Delete

```go
func Delete(ctx context.Context, store *tinystore.Store, name string, labels Labels) (bool, error)
```

Delete removes the one series of exactly this name and these labels, whether its samples still read or not, and says whether the store held it; nil labels are the series without any.

## Describe

```go
func Describe(ctx context.Context, store *tinystore.Store, name string, description Description) error
```

Describe sets a name's unit and help in place of what it had: what is not given is removed. They belong to the name, so a Delete of its series leaves them.

## ExponentialBuckets

```go
func ExponentialBuckets(start, factor float64, count int) []float64
```

ExponentialBuckets is upper bounds from start, each factor times the one before, as Prometheus' ExponentialBuckets: ExponentialBuckets(1, 2, 14) is 1, 2, 4 … 8192.

## Flush

```go
func Flush(ctx context.Context, store *tinystore.Store) error
```

Flush sends what every instrument changed and has the store write its totals now, as it does every 15 seconds. One the store cannot be reached for fails as a call does, and what it carried goes once it can be; what the store refused since the flush before, and what a store that started again may have counted, fail it, Invalid and OutcomeUnknown, each once.

## Ingest

```go
func Ingest(ctx context.Context, store *tinystore.Store, series ...Series) error
```

Ingest writes series and their samples, every one or none: a refused series names itself by its labels. It returns once they are written.

## OpenGaugeFunc

```go
func OpenGaugeFunc(store *tinystore.Store, name string, read func(ctx context.Context) (float64, error), opts ...Option) error
```

OpenGaugeFunc is a gauge whose value read gives every 15 seconds and at Flush, of the set of labels Labels gives, none unless given. Another set of the name takes a function of its own. It is never called again while a call of it runs, and an error or a value that is no finite number writes nothing that time, said once while it repeats.

	err = metrics.OpenGaugeFunc(store, "queue_depth", emails.Depth, metrics.Labels{"queue": "emails"})

## Bucket

```go
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
```

Bucket is a bucket of an aggregate. One of count … last has its Value; a percentile's is a gauge's sample at the rank, or a timer's bucket of its spread that holds it, from Lower to Upper, and with Estimate asked a point between them.

## Condition

```go
type Condition struct {
	// contains filtered or unexported fields
}
```

Condition is what a label's value must be beside a value of its own: one of a list, none of one, or a prefix.

### Not

```go
func Not(values ...any) Condition
```

Not is a label equal to none of these, or absent.

### OneOf

```go
func OneOf(values ...any) Condition
```

OneOf is a label equal to one of these.

### Prefix

```go
func Prefix(text string) Condition
```

Prefix is a label that starts with this text, in a query; in a listing, the names, labels or values that do.

## Counter

```go
type Counter struct {
	// contains filtered or unexported fields
}
```

Counter is a total that only goes up.

### OpenCounter

```go
func OpenCounter(store *tinystore.Store, name string, opts ...Option) (*Counter, error)
```

OpenCounter is a total that only goes up, which the store keeps: processes counting into one series count into one total. Another form or other options than the name has while the store is open is Invalid.

### Counter.Add

```go
func (c *Counter) Add(n float64)
```

Add adds n, a finite number of 0 or more; Add(0) makes the set of labels known with a total of zero.

### Counter.Inc

```go
func (c *Counter) Inc()
```

Inc adds one.

### Counter.With

```go
func (c *Counter) With(pairs ...any) *Counter
```

With is the same counter with more labels, given in pairs, a series of its own; the same labels in any order are one.

## Damage

```go
type Damage struct {
	Name   string
	Labels Labels
	Error  string
}
```

Damage is a series whose bytes no longer read, which Delete removes.

### Damaged

```go
func Damaged(ctx context.Context, store *tinystore.Store) ([]Damage, error)
```

Damaged is the series whose bytes no longer read, 1000 at most, each with what broke; deleting them shows the next.

## Description

```go
type Description struct {
	Unit, Help string
}
```

Description is a name's unit and help; both empty when nothing described it.

### DescriptionOf

```go
func DescriptionOf(ctx context.Context, store *tinystore.Store, name string) (Description, error)
```

DescriptionOf is a name's unit and help.

## Exceeds

```go
type Exceeds struct {
	// Limit is series, bytes read or samples decoded.
	Limit         string
	Wanted, Bound int64
}
```

Exceeds is the bound a call would stop at, as its Limit error names it.

## Gauge

```go
type Gauge struct {
	// contains filtered or unexported fields
}
```

Gauge is a value that goes up and down.

### OpenGauge

```go
func OpenGauge(store *tinystore.Store, name string, opts ...Option) (*Gauge, error)
```

OpenGauge is a value that goes up and down, written while this program is connected: a queue's length, a temperature.

### Gauge.Add

```go
func (g *Gauge) Add(n float64)
```

Add moves it by n, from zero when it was never set.

### Gauge.Dec

```go
func (g *Gauge) Dec()
```

### Gauge.Inc

```go
func (g *Gauge) Inc()
```

### Gauge.Set

```go
func (g *Gauge) Set(value float64)
```

Set sets its value, a finite number.

### Gauge.With

```go
func (g *Gauge) With(pairs ...any) *Gauge
```

With is the same gauge with more labels, given in pairs, a series of its own.

## Group

```go
type Group struct {
	Name string
	// Labels is the labels By kept or Without left.
	Labels  Labels
	Buckets []Bucket
}
```

Group is the buckets of a group of series: a bucket without samples is left out.

### Aggregate

```go
func Aggregate(ctx context.Context, store *tinystore.Store, q Query) ([]Group, error)
```

Aggregate is the buckets of each group of the series the query finds, each one exact value of its Op, rounded once.

## Histogram

```go
type Histogram struct {
	// contains filtered or unexported fields
}
```

Histogram is how many numbers of what size: their count, sum and largest, and their spread.

### OpenHistogram

```go
func OpenHistogram(store *tinystore.Store, name string, opts ...Option) (*Histogram, error)
```

OpenHistogram is a timer of numbers that are not durations: a response's size in bytes.

### Histogram.Record

```go
func (h *Histogram) Record(value float64)
```

Record records a value of 0 or more.

### Histogram.With

```go
func (h *Histogram) With(pairs ...any) *Histogram
```

With is the same histogram with more labels, given in pairs, a series of its own.

## Kind

```go
type Kind string
```

Kind is what a series' samples are.

### KindGauge

```go
const (
	KindGauge   Kind = "gauge"
	KindCounter Kind = "counter"
	// KindHistogram is the _bucket series of a histogram written elsewhere, each with its le.
	KindHistogram Kind = "histogram"
)
```

## Labels

```go
type Labels map[string]string
```

Labels are a series' labels, by name.

## Name

```go
type Name struct {
	Name        string
	Kind        Kind
	SeriesCount int
	Unit, Help  string
	Keep        time.Duration
	// RetainedFrom is its oldest sample, or where its keep begins while it
	// holds samples from before, which no read finds.
	RetainedFrom time.Time
	Newest       time.Time
}
```

Name is a name the store holds samples of.

## NamePage

```go
type NamePage struct {
	Names []Name
	Next  string
}
```

NamePage is a page of names and where the next starts: "" on the last.

### Names

```go
func Names(ctx context.Context, store *tinystore.Store, opts ...PageOption) (NamePage, error)
```

Names is a page of the names the store holds samples of, in the byte order of their text; none of it reads a sample.

## Op

```go
type Op string
```

Op is what an aggregate's bucket answers.

### Count

```go
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
```

### Percentile

```go
func Percentile(rank float64) Op
```

Percentile is a gauge's sample at this rank, or a timer's bucket of its spread that holds it: Percentile(95), Percentile(99.9).

## Option

```go
type Option interface {
	// contains filtered or unexported methods
}
```

Option is what an instrument is told.

### Buckets

```go
func Buckets(bounds ...float64) Option
```

Buckets are a timer's or a histogram's upper bounds, ascending; the store adds +Inf, and a percentile of it becomes possible.

### Exponential

```go
func Exponential() Option
```

Exponential lets the store keep a timer's spread on Prometheus' native grid, eight intervals a doubling, an experiment: a percentile then lies within one interval, with no bound to choose.

### Help

```go
func Help(text string) Option
```

Help is a line on what the name means, 1024 bytes at most.

### Keep

```go
func Keep(d time.Duration) Option
```

Keep is how long the name's samples stay, kept in the file with it: 30 days unless a name was given one.

### Unit

```go
func Unit(text string) Option
```

Unit is the unit of its values, 32 bytes at most: "bytes", "%"; a timer's is ms.

## PageOption

```go
type PageOption interface {
	// contains filtered or unexported methods
}
```

PageOption is how a page of names, labels or values reads: Prefix, After, PageLimit.

### After

```go
func After(next string) PageOption
```

After starts a page after the page before's Next.

### PageLimit

```go
func PageLimit(n int) PageOption
```

PageLimit is how many a page holds: 1000 unless given, 10,000 at most.

## Plan

```go
type Plan struct {
	Series, Bytes, Decoded int64
	// Exceeds is nil when the call fits every bound.
	Exceeds *Exceeds
}
```

Plan is what a read or an aggregate would spend of each bound.

### Explain

```go
func Explain(ctx context.Context, store *tinystore.Store, q Query) (Plan, error)
```

Explain is what Read, or Aggregate when given an Op, would spend of each bound; it decodes no sample.

## Point

```go
type Point struct {
	Name   string
	Labels Labels
	At     time.Time
	Value  float64
}
```

Point is a series' newest sample in the range.

### Latest

```go
func Latest(ctx context.Context, store *tinystore.Store, q Query) ([]Point, error)
```

Latest is the newest sample of each series the query finds, when it has one in the range: a series without one is left out.

## Query

```go
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
	Op    Op
	// By is the labels a group keeps; without By and Without, every series of a name is one group.
	By []string
	// Without is the labels a group leaves out, keeping every other.
	Without []string
	// Estimate gives a timer's percentile a point between its bounds, interpolated.
	Estimate bool
}
```

Query is the series a call finds and its range, and for an aggregate how it cuts and groups them. A query needs a Name, or a label's value, a list or a prefix in Where.

## Sample

```go
type Sample struct {
	At    time.Time
	Value float64
}
```

Sample is a time, kept to the millisecond, and a number, kept bit for bit.

## Series

```go
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
```

Series is a series: what Ingest writes, and what a read gives back. Ingest takes its samples as Samples, or as Times and Values, two columns of one length; a read gives Samples.

### Read

```go
func Read(ctx context.Context, store *tinystore.Store, q Query) ([]Series, error)
```

Read is the samples of each series the query finds, in its range: whole, or a Limit error naming its bound, never fewer.

## TextPage

```go
type TextPage struct {
	Texts []string
	Next  string
}
```

TextPage is a page of a name's labels or a label's values, and where the next starts.

### LabelNames

```go
func LabelNames(ctx context.Context, store *tinystore.Store, name string, opts ...PageOption) (TextPage, error)
```

LabelNames is a page of a name's label names, but the store's own.

### LabelValues

```go
func LabelValues(ctx context.Context, store *tinystore.Store, name, label string, opts ...PageOption) (TextPage, error)
```

LabelValues is a page of the values a label of a name's series holds.

## Timer

```go
type Timer struct {
	// contains filtered or unexported fields
}
```

Timer is how many things took how long.

### OpenTimer

```go
func OpenTimer(store *tinystore.Store, name string, opts ...Option) (*Timer, error)
```

OpenTimer is how many things took how long: their count, sum and longest in milliseconds, and their spread with Buckets. A timer named http\_request\_ms writes http\_request\_ms\_count, \_sum, \_max and, with buckets, \_bucket.

### Timer.Record

```go
func (t *Timer) Record(d time.Duration)
```

Record records a duration measured elsewhere.

### Timer.Start

```go
func (t *Timer) Start() *Timing
```

Start is a timing that records when it stops, for a span that is no one call: defer latency.Start().Stop().

### Timer.With

```go
func (t *Timer) With(pairs ...any) *Timer
```

With is the same timer with more labels, given in pairs, a series of its own; le is its \_bucket series'.

## Timing

```go
type Timing struct {
	// contains filtered or unexported fields
}
```

Timing is a span a timer measures from its start.

### Timing.Stop

```go
func (t *Timing) Stop() time.Duration
```

Stop records how long since the timing started, the first time it is called, and gives it.

## Where

```go
type Where map[string]any
```

Where is what a query finds of each label: a value, a number or a bool as its text, or a Condition.

<!-- Generated by task reference from sdk/go/metrics/. Edit the doc comments there, not this file. -->
