package metrics

import (
	"errors"
	"time"

	"github.com/tinyshed/tinystore/codec"
)

var (
	ErrInvalid      = errors.New("invalid metrics request")
	ErrLimit        = errors.New("metrics resource limit")
	ErrClosed       = errors.New("metrics store is closed")
	ErrTooOld       = errors.New("sample is expired or sealed")
	ErrConflict     = errors.New("metrics state changed")
	ErrCorrupt      = errors.New("corrupt metrics data")
	ErrSuspended    = errors.New("metrics maintenance is suspended for this series")
	ErrNonFinite    = errors.New("nonfinite metrics aggregate input")
	ErrCounterValue = errors.New("invalid counter aggregate input")
)

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

// Range selects exact labels and an exclusive upper timestamp bound; limits may only narrow the store's limits.
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

type AggregateBucket struct {
	From, To      int64
	Count, Resets int
	Value         float64
	Overflow      bool
}

type AggregateResult struct {
	Series  Series
	Buckets []AggregateBucket
}

type Maintenance struct{ SealedBlocks, ExpiredSamples, Conflicts, QuarantinedSeries, ReclaimedSeries int }

// Stats counts this handle's work; QuarantinedSeries is the current persisted count.
type Stats struct{ IngestedSamples, RejectedBatches, Queries, SealedBlocks, ExpiredSamples, QuarantinedSeries, ReclaimedSeries uint64 }

// MaintenanceFailure is a persisted diagnostic; SeriesID is a file-local cursor, not a series handle.
type MaintenanceFailure struct {
	SeriesID int64
	FailedAt int64
	Reason   string
}
