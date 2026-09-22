// Package metrics stores exact numeric series in one durable SQLite file.
package metrics

import (
	"errors"
	"time"

	"github.com/tinyshed/tinystore/codec"
)

var (
	ErrInvalid   = errors.New("invalid metrics request")
	ErrLimit     = errors.New("metrics resource limit")
	ErrClosed    = errors.New("metrics store is closed")
	ErrTooOld    = errors.New("sample is expired or sealed")
	ErrConflict  = errors.New("metrics state changed")
	ErrCorrupt   = errors.New("corrupt metrics data")
	ErrSuspended = errors.New("metrics maintenance is suspended for this series")
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

type Limits struct {
	Series         int
	Blocks         int
	PayloadBytes   int
	DecodedSamples int
	OutputSamples  int
}

type Options struct {
	Retention           time.Duration
	Lateness            time.Duration
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
	Limits              Limits
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

type Maintenance struct{ SealedBlocks, ExpiredSamples, Conflicts, QuarantinedSeries, ReclaimedSeries int }

// Stats counts this handle's work; QuarantinedSeries is the current persisted count.
type Stats struct{ IngestedSamples, RejectedBatches, Queries, SealedBlocks, ExpiredSamples, QuarantinedSeries, ReclaimedSeries uint64 }

// MaintenanceFailure is a persisted diagnostic; SeriesID is a file-local cursor, not a series handle.
type MaintenanceFailure struct {
	SeriesID int64
	FailedAt int64
	Reason   string
}
