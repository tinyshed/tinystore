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

func (s *sentinel) Error() string { return s.message }
func (s *sentinel) Unwrap() error { return s.kind }

// SeriesError is a refusal that belongs to one series: its data, state or
// limits, never a cancelled call or a failing file. errors.Is still finds the
// cause and errors.As gives the labels; Ingest is atomic, so a caller can send
// the call again without that series.
type SeriesError struct {
	Labels []Label
	Err    error
}

func (e *SeriesError) Error() string {
	return "series " + formatLabels(e.Labels) + ": " + e.Err.Error()
}

func (e *SeriesError) Unwrap() error { return e.Err }

type Sample = codec.Sample

type Kind string

const (
	Gauge   Kind = "gauge"
	Counter Kind = "counter"
)

type Label struct{ Name, Value string }

type Series struct {
	Labels []Label
	Kind   Kind
}

type Batch struct {
	Series  Series
	Samples []Sample
}

// Range selects exact labels and an exclusive upper timestamp bound; limits may
// only narrow the store's limits.
type Range struct {
	Matchers []Label
	From, To int64
	Limits   Limits
}

type Result struct {
	Series  Series
	Samples []Sample
}

type AggregateOp string

const (
	AggregateCount    AggregateOp = "count"
	AggregateSum      AggregateOp = "sum"
	AggregateMin      AggregateOp = "min"
	AggregateMax      AggregateOp = "max"
	AggregateIncrease AggregateOp = "increase"
)

type AggregateRequest struct {
	Range Range
	Width time.Duration
	Op    AggregateOp
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
