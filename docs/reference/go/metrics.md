# Metrics API for Go

Every public type, function and constant of the Metrics engine in `github.com/tinyshed/tinystore/metrics`, generated from its source. The [Metrics guide](../../metrics/README.md) explains how to use them, and the [Bun and Node](../bun/metrics.md) and [Python](../python/metrics.md) pages list the same API.

## LimitSeries

```go
const (
	LimitSeries         = "matched series"
	LimitBlocks         = "decoded blocks"
	LimitPayloadBytes   = "fetched bytes"
	LimitDecodedSamples = "decoded samples"
	LimitOutputSamples  = "output samples"
	LimitOutputBuckets  = "output buckets"
)
```

The names of a query's budgets, which a LimitError's Name holds when one is spent.

## ErrInvalid

```go
var (
	ErrInvalid      = kindOf("invalid metrics request", tinystore.ErrInvalid)
	ErrLimit        = kindOf("metrics resource limit", tinystore.ErrLimit)
	ErrClosed       = kindOf("metrics store is closed", tinystore.ErrClosed)
	ErrTooOld       = kindOf("sample is expired or sealed", tinystore.ErrTooOld)
	ErrTooNew       = kindOf("sample is ahead of the store's clock", tinystore.ErrTooNew)
	ErrConflict     = kindOf("metrics state changed", tinystore.ErrConflict)
	ErrCorrupt      = kindOf("corrupt metrics data", tinystore.ErrCorrupt)
	ErrSuspended    = kindOf("metrics maintenance is suspended for this series", tinystore.ErrSuspended)
	ErrNonFinite    = kindOf("nonfinite metrics aggregate input", tinystore.ErrInvalid)
	ErrCounterValue = kindOf("invalid counter aggregate input", tinystore.ErrInvalid)
)
```

Each wraps the store's sentinel of the same kind, so errors.Is(err, tinystore.ErrLimit) holds for a metrics limit as it does in every engine.

## AggregateBucket

```go
type AggregateBucket struct {
	From, To      int64
	Count, Resets int
	Value         float64
	Overflow      bool

	// Partial reports that retention cut the bucket: only its samples from the
	// cutoff on were counted.
	Partial bool
}
```

AggregateBucket keeps the edges it was asked for.

## AggregateOp

```go
type AggregateOp string
```

### AggregateCount

```go
const (
	AggregateCount    AggregateOp = "count"
	AggregateSum      AggregateOp = "sum"
	AggregateMin      AggregateOp = "min"
	AggregateMax      AggregateOp = "max"
	AggregateAvg      AggregateOp = "avg"
	AggregateIncrease AggregateOp = "increase"
	AggregateRate     AggregateOp = "rate"
	AggregateDelta    AggregateOp = "delta"
)
```

Each operation is computed exactly and rounded once, a group's too:

	avg       the mean of the bucket's samples, every series of a group weighed by them
	increase  a counter's rise, its resets counted
	rate      a counter's increase a second of the bucket
	delta     a gauge's last sample less its first

## AggregateRequest

```go
type AggregateRequest struct {
	Range       Range
	Width       time.Duration
	Op          AggregateOp
	By, Without []string
}
```

AggregateRequest asks for a Range's buckets Width long. By groups the matched series by the values of these labels, Without by every label but these, each group one result; neither keeps every series apart. A group never joins two names or two kinds:

	Op: AggregateIncrease, By: []string{"route"}   http_requests_total{route="/a"}, …{route="/b"}

## AggregateResult

```go
type AggregateResult struct {
	Series  Series
	Buckets []AggregateBucket
}
```

## Batch

```go
type Batch struct {
	Series  Series
	Samples []Sample
}
```

## Condition

```go
type Condition struct {
	// contains filtered or unexported fields
}
```

Condition is what a label's value must be for a range to take a series, beyond the equality of Match:

	Where{"status": OneOf("500", "502")}   status is 500 or 502
	Where{"env": NoneOf("dev", "test")}    env is neither, or the series has none
	Where{"host": Prefix("api-")}          host begins with api-

A range keeps something that finds its series, a Name, a Match, a OneOf or a Prefix: NoneOf only leaves series out, since alone it would scan them all.

### NoneOf

```go
func NoneOf(values ...string) Condition
```

### OneOf

```go
func OneOf(values ...string) Condition
```

### Prefix

```go
func Prefix(prefix string) Condition
```

## CounterInstrument

```go
type CounterInstrument struct {
	// contains filtered or unexported fields
}
```

CounterInstrument only grows. Its total since the process started is ingested as a counter sample at each flush, so a restart is a reset Aggregate counts.

### CounterInstrument.Add

```go
func (c CounterInstrument) Add(n float64)
```

Add grows the counter by n; a negative or non-finite n is dropped and logged once, since a counter that shrinks would read as a reset.

### CounterInstrument.Inc

```go
func (c CounterInstrument) Inc()
```

### CounterInstrument.With

```go
func (c CounterInstrument) With(labels ...string) CounterInstrument
```

With is the counter of the same name with more labels, given as name, value pairs; the same labels in any order are the same series.

## DroppedSeries

```go
type DroppedSeries struct {
	Found bool

	// UnreadableGroups counts the groups removed without the payload rows their
	// directory named.
	UnreadableGroups int
}
```

DroppedSeries is what DropSeries removed.

## GaugeInstrument

```go
type GaugeInstrument struct {
	// contains filtered or unexported fields
}
```

GaugeInstrument is a value set or moved by the application, ingested at each flush.

### GaugeInstrument.Add

```go
func (g GaugeInstrument) Add(delta float64)
```

### GaugeInstrument.Set

```go
func (g GaugeInstrument) Set(value float64)
```

### GaugeInstrument.With

```go
func (g GaugeInstrument) With(labels ...string) GaugeInstrument
```

## Kind

```go
type Kind string
```

### Gauge

```go
const (
	Gauge   Kind = "gauge"
	Counter Kind = "counter"
)
```

## Labels

```go
type Labels map[string]string
```

Labels tell the series of one name apart: a host, a route, a status. A label whose name begins with \_\_ is the store's own, and refused.

## Limits

```go
type Limits struct {
	Series         int
	Blocks         int
	PayloadBytes   int
	DecodedSamples int
	OutputSamples  int
}
```

## Maintenance

```go
type Maintenance struct{ SealedBlocks, ExpiredSamples, Conflicts, QuarantinedSeries, ReclaimedSeries int }
```

## MaintenanceFailure

```go
type MaintenanceFailure struct {
	// SeriesID is a file-local cursor, not a series handle.
	SeriesID int64

	FailedAt int64
	Reason   string
}
```

MaintenanceFailure is a persisted diagnostic.

## Options

```go
type Options struct {
	Retention           time.Duration
	Lateness            time.Duration
	ClockSkew           time.Duration
	MaxBlockSpan        time.Duration
	MaxSeries           int
	MaxHeadSamples      int
	MaxHeadBytes        int
	MaxBatchSamples     int
	MaxBatchBytes       int
	MaintenanceSeries   int
	MaxReaders          int
	MaxConcurrentReads  int
	MaxConcurrentIngest int
	SnapshotTimeout     time.Duration
	MaintenanceInterval time.Duration
	Flush               time.Duration
	Limits              Limits
}
```

## Plan

```go
type Plan struct {
	Series, Blocks, Summarized, PayloadBytes, DecodedSamples int
	Limits                                                   Limits
	Stops                                                    *tinystore.LimitError
}
```

Plan is what a Read or an Aggregate would spend of its limits, found in one snapshot from the series, their block directories and their heads, with no block's payload fetched and no sample decoded. Each use stands beside the limit it is held to, and Stops is the limit the call would reach, nil when it fits; the plan stops counting there, as the call would stop working.

	Series 3, Blocks 40, Summarized 36, PayloadBytes 12288, DecodedSamples 960
	  → an hourly aggregate answers 36 blocks from their summaries, decodes 4

The samples a Read answers are not known before its blocks are decoded, so a plan does not count them.

## Range

```go
type Range struct {
	Name     string
	Match    Labels
	Where    Where
	Since    time.Duration
	From, To int64 // unix milliseconds
	Limits   Limits
}
```

Range selects the series of a name, of labels matched exactly, of labels under conditions, or of any of them together, and their samples in \[From, To) or over the last Since; a To of zero is the open end. Limits may only narrow the store's.

	Range{Name: "cpu", Since: time.Hour}                   the last hour of every cpu series
	Range{Match: Labels{"host": "web-1"}, From: f, To: t}  every series of web-1, f to t
	Range{Name: "http_requests_total", Where: Where{"status": OneOf("500", "502")}, Since: time.Hour}

## Result

```go
type Result struct {
	Series  Series
	Samples []Sample
}
```

## Sample

```go
type Sample = codec.Sample
```

## Series

```go
type Series struct {
	Name   string
	Kind   Kind
	Labels Labels
}
```

Series is what a metric's samples are, by its name and its labels:

	http_requests_total{route="/users", status="200"}, a counter

### Series.String

```go
func (s Series) String() string
```

String prints the series as Prometheus does: cpu{host="web-1"}.

## SeriesError

```go
type SeriesError struct {
	Name   string
	Labels Labels
	Err    error
}
```

SeriesError is a refusal that belongs to one series: its data, state or limits, never a cancelled call or a failing file. errors.Is still finds the cause and errors.As gives the series' name and labels; Ingest is atomic, so a caller can send the call again without that series.

### SeriesError.Error

```go
func (e *SeriesError) Error() string
```

### SeriesError.Unwrap

```go
func (e *SeriesError) Unwrap() error
```

## Stats

```go
type Stats struct {
	IngestedSamples uint64
	RejectedBatches uint64
	Queries         uint64
	SealedBlocks    uint64
	ExpiredSamples  uint64

	// QuarantinedSeries is the current persisted count, not this handle's work.
	QuarantinedSeries uint64

	ReclaimedSeries uint64
}
```

Stats counts this handle's work.

## Store

```go
type Store struct {
	// contains filtered or unexported fields
}
```

### Open

```go
func Open(ctx context.Context, runtime *tinystore.Store, options Options) (*Store, error)
```

Open opens metrics.db inside the store. The store closes it, and unless it is Manual runs Maintain every Options.MaintenanceInterval, a minute by default.

### Store.Aggregate

```go
func (s *Store) Aggregate(ctx context.Context, request AggregateRequest) ([]AggregateResult, error)
```

Aggregate rounds exact finite arithmetic once and never uses legacy float64 block sums.

### Store.Close

```go
func (s *Store) Close(ctx context.Context) error
```

Close ingests the instruments' last values, stops admission and drains in-flight work; cancellation stops waiting, not cleanup. The store calls it: an application closes the store instead.

### Store.Counter

```go
func (s *Store) Counter(name string) CounterInstrument
```

### Store.DropSeries

```go
func (s *Store) DropSeries(ctx context.Context, name string, labels Labels) (DroppedSeries, error)
```

DropSeries removes one series and everything it holds in one transaction, whether its data still reads or not, including a suspended series. A group whose directory or clock no longer reads is removed without the payload rows it named: they stay in the file, and UnreadableGroups counts such groups.

### Store.ExplainAggregate

```go
func (s *Store) ExplainAggregate(ctx context.Context, request AggregateRequest) (Plan, error)
```

ExplainAggregate is the Plan of Aggregate(ctx, request), whose whole blocks inside one bucket are answered from their summaries.

### Store.ExplainRead

```go
func (s *Store) ExplainRead(ctx context.Context, request Range) (Plan, error)
```

ExplainRead is the Plan of Read(ctx, request).

### Store.Flush

```go
func (s *Store) Flush(ctx context.Context) error
```

Flush ingests every instrument's value now. A series Ingest refuses, an invalid label say, is dropped and logged once rather than keeping the rest out: Ingest is atomic, so the flush is retried without it. A flush that fails keeps the longest durations it took for the next.

### Store.Gauge

```go
func (s *Store) Gauge(name string) GaugeInstrument
```

### Store.GaugeFunc

```go
func (s *Store) GaugeFunc(name string, read func(context.Context) (float64, error))
```

GaugeFunc asks read for the gauge's value at each flush; an error skips that sample and is logged, once while it stays the same.

### Store.Ingest

```go
func (s *Store) Ingest(ctx context.Context, batches []Batch) (err error)
```

Ingest stores every batch or none of them. A timestamp repeated within the call, or already waiting in the head, keeps the value supplied last.

### Store.ListMaintenanceFailures

```go
func (s *Store) ListMaintenanceFailures(ctx context.Context, afterID int64) ([]MaintenanceFailure, error)
```

ListMaintenanceFailures returns one bounded page ordered by file-local series id.

### Store.Maintain

```go
func (s *Store) Maintain(ctx context.Context) (Maintenance, error)
```

Maintain performs a bounded retention pass, then seals eligible full microblocks.

### Store.Read

```go
func (s *Store) Read(ctx context.Context, request Range) ([]Result, error)
```

Read returns owned samples, or an error with no partial result; the caller holds no SQLite snapshot.

### Store.Report

```go
func (s *Store) Report() []tinystore.Measure
```

### Store.RetryFailedMaintenance

```go
func (s *Store) RetryFailedMaintenance(ctx context.Context) (int, error)
```

RetryFailedMaintenance re-enables at most MaintenanceSeries suspended series per call.

### Store.Snapshot

```go
func (s *Store) Snapshot(ctx context.Context, dir string) ([]tinystore.SnapshotFile, error)
```

Snapshot copies metrics.db into dir while the engine keeps working.

### Store.Stats

```go
func (s *Store) Stats() Stats
```

### Store.Stream

```go
func (s *Store) Stream(ctx context.Context, request Range, yield func(Result) error) error
```

Stream passes one owned series at a time after the snapshot closes; an error may follow earlier results.

### Store.Timer

```go
func (s *Store) Timer(name string) TimerInstrument
```

### Store.WriteSelf

```go
func (s *Store) WriteSelf(ctx context.Context, measures []tinystore.Measure) error
```

WriteSelf keeps diagnostics in ordinary series without recursively counting their own ingest. Caller counters above 2^53 are refused, never rounded.

## TimerInstrument

```go
type TimerInstrument struct {
	// contains filtered or unexported fields
}
```

TimerInstrument measures how long something takes. Each flush ingests how many durations it measured and their sum in milliseconds, since the process started, as the counters \&lt;name>\_count and \&lt;name>\_sum, and the longest since the flush before as the gauge \&lt;name>\_max, left out when it measured none. A range's mean is the increase of its sum over the increase of its count.

### TimerInstrument.Record

```go
func (t TimerInstrument) Record(d time.Duration)
```

Record adds one duration; a negative one is dropped and logged once.

### TimerInstrument.Since

```go
func (t TimerInstrument) Since(start time.Time)
```

Since records the time from start to now, so that a deferred call times the function it is deferred in: defer latency.Since(time.Now()).

### TimerInstrument.With

```go
func (t TimerInstrument) With(labels ...string) TimerInstrument
```

With is the timer of the same name with more labels, given as name, value pairs; the same labels in any order are the same series.

## Where

```go
type Where map[string]Condition
```

Where is the conditions of a range, a label's name to its condition.

<!-- Generated by task reference from metrics/. Edit the doc comments there, not this file. -->
