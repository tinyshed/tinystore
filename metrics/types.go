package metrics

import (
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/codec"
)

// Each wraps the store's sentinel of the same kind, so errors.Is(err,
// tinystore.ErrLimit) holds for a metrics limit as it does in every engine.
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

// sentinel keeps the engine's own message while errors.Is finds the store's kind
type sentinel struct {
	message string
	kind    error
}

func kindOf(message string, kind error) error { return &sentinel{message: message, kind: kind} }

// The names of a query's budgets, which a LimitError's Name holds when one is spent.
const (
	LimitSeries         = "matched series"
	LimitBlocks         = "decoded blocks"
	LimitPayloadBytes   = "fetched bytes"
	LimitDecodedSamples = "decoded samples"
	LimitOutputSamples  = "output samples"
	LimitOutputBuckets  = "output buckets"
)

// limit is a metrics limit a query reached, named, which errors.Is finds as
// ErrLimit and the store's.
func limit(name string, wanted, bound int) error {
	return &tinystore.LimitError{Name: name, Wanted: int64(wanted), Bound: int64(bound), Kind: ErrLimit}
}

func (s *sentinel) Error() string { return s.message }
func (s *sentinel) Unwrap() error { return s.kind }

// SeriesError is a refusal that belongs to one series: its data, state or
// limits, never a cancelled call or a failing file. errors.Is still finds the
// cause and errors.As gives the series' name and labels; Ingest is atomic, so a
// caller can send the call again without that series.
type SeriesError struct {
	Name   string
	Labels Labels
	Err    error
}

func (e *SeriesError) Error() string {
	return "series " + formatSeries(e.Name, e.Labels) + ": " + e.Err.Error()
}

func (e *SeriesError) Unwrap() error { return e.Err }

type Sample = codec.Sample

type Kind string

const (
	Gauge   Kind = "gauge"
	Counter Kind = "counter"
)

// Labels tell the series of one name apart: a host, a route, a status. A label
// whose name begins with __ is the store's own, and refused.
type Labels map[string]string

// Series is what a metric's samples are, by its name and its labels:
//
//	http_requests_total{route="/users", status="200"}, a counter
type Series struct {
	Name   string
	Kind   Kind
	Labels Labels
}

// String prints the series as Prometheus does: cpu{host="web-1"}.
func (s Series) String() string { return formatSeries(s.Name, s.Labels) }

type Batch struct {
	Series  Series
	Samples []Sample
}

// Range selects the series of a name, of labels matched exactly, of labels
// under conditions, or of any of them together, and their samples in
// [From, To) or over the last Since; a To of zero is the open end. Limits may
// only narrow the store's.
//
//	Range{Name: "cpu", Since: time.Hour}                   the last hour of every cpu series
//	Range{Match: Labels{"host": "web-1"}, From: f, To: t}  every series of web-1, f to t
//	Range{Name: "http_requests_total", Where: Where{"status": OneOf("500", "502")}, Since: time.Hour}
type Range struct {
	Name     string
	Match    Labels
	Where    Where
	Since    time.Duration
	From, To int64 // unix milliseconds
	Limits   Limits
}

type Result struct {
	Series  Series
	Samples []Sample
}

type AggregateOp string

// Each operation is computed exactly and rounded once, a group's too:
//
//	avg       the mean of the bucket's samples, every series of a group weighed by them
//	increase  a counter's rise, its resets counted
//	rate      a counter's increase a second of the bucket
//	delta     a gauge's last sample less its first
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

// AggregateRequest asks for a Range's buckets Width long. By groups the
// matched series by the values of these labels, Without by every label but
// these, each group one result; neither keeps every series apart. A group
// never joins two names or two kinds:
//
//	Op: AggregateIncrease, By: []string{"route"}   http_requests_total{route="/a"}, …{route="/b"}
type AggregateRequest struct {
	Range       Range
	Width       time.Duration
	Op          AggregateOp
	By, Without []string
}

// AggregateBucket keeps the edges it was asked for.
type AggregateBucket struct {
	From, To      int64
	Count, Resets int
	Value         float64
	Overflow      bool

	// Partial reports that retention cut the bucket: only its samples from the
	// cutoff on were counted.
	Partial bool
}

type AggregateResult struct {
	Series  Series
	Buckets []AggregateBucket
}

// DroppedSeries is what DropSeries removed.
type DroppedSeries struct {
	Found bool

	// UnreadableGroups counts the groups removed without the payload rows their
	// directory named.
	UnreadableGroups int
}

type Maintenance struct{ SealedBlocks, ExpiredSamples, Conflicts, QuarantinedSeries, ReclaimedSeries int }

// Stats counts this handle's work.
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

// MaintenanceFailure is a persisted diagnostic.
type MaintenanceFailure struct {
	// SeriesID is a file-local cursor, not a series handle.
	SeriesID int64

	FailedAt int64
	Reason   string
}
