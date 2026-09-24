package metrics

import (
	"fmt"
	"math"
	"time"
)

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
	SharedBudget        *WorkBudget
	Limits              Limits
}

func normalizeOptions(o Options) (Options, error) {
	if o.SharedBudget != nil && o.SharedBudget.capacity <= 0 {
		return o, fmt.Errorf("%w: uninitialized shared work budget", ErrInvalid)
	}
	if o.Retention == 0 {
		o.Retention = 30 * 24 * time.Hour
	}
	if o.SnapshotTimeout == 0 {
		o.SnapshotTimeout = 5 * time.Second
	}
	if o.MaxBlockSpan == 0 {
		o.MaxBlockSpan = 24 * time.Hour
	}
	if o.MaxBlockSpan < time.Millisecond || o.MaxBlockSpan%time.Millisecond != 0 {
		return o, fmt.Errorf("%w: block span", ErrInvalid)
	}
	if o.Retention < time.Millisecond || o.Retention%time.Millisecond != 0 || o.Lateness < 0 || o.Lateness%time.Millisecond != 0 || o.SnapshotTimeout < 0 {
		return o, fmt.Errorf("%w: durations", ErrInvalid)
	}
	fields := []*int{&o.MaxSeries, &o.MaxHeadSamples, &o.MaxBatchSamples, &o.MaxBatchBytes, &o.MaintenanceSeries, &o.MaxReaders}
	defaults := []int{100000, 4096, 10000, 4 << 20, 64, 2}
	for i, p := range fields {
		if *p < 0 {
			return o, fmt.Errorf("%w: negative capacity", ErrInvalid)
		}
		if *p == 0 {
			*p = defaults[i]
		}
	}
	if o.MaxConcurrentReads < 0 || o.MaxConcurrentIngest < 0 || o.MaxConcurrentReads > 1<<16 || o.MaxConcurrentIngest > 1<<16 {
		return o, fmt.Errorf("%w: concurrent work capacity", ErrInvalid)
	}
	if o.MaxConcurrentReads == 0 {
		o.MaxConcurrentReads = o.MaxReaders
	}
	if o.MaxConcurrentIngest == 0 {
		o.MaxConcurrentIngest = 1
	}
	if o.MaxHeadBytes == 0 {
		o.MaxHeadBytes = 256 << 10
	}
	if o.MaxHeadBytes < 1 || o.MaxHeadBytes > maximumHeadBytes || o.MaxHeadSamples > 1<<20 {
		return o, fmt.Errorf("%w: mutable head capacity", ErrInvalid)
	}
	defaultsLimits := Limits{Series: 1000, Blocks: 4096, PayloadBytes: 16 << 20, DecodedSamples: 1 << 20, OutputSamples: 100000}
	configured := []*int{&o.Limits.Series, &o.Limits.Blocks, &o.Limits.PayloadBytes, &o.Limits.DecodedSamples, &o.Limits.OutputSamples}
	limitDefaults := []int{defaultsLimits.Series, defaultsLimits.Blocks, defaultsLimits.PayloadBytes, defaultsLimits.DecodedSamples, defaultsLimits.OutputSamples}
	for i, capacity := range configured {
		if *capacity < 0 || *capacity == math.MaxInt {
			return o, fmt.Errorf("%w: query capacity", ErrInvalid)
		}
		if *capacity == 0 {
			*capacity = limitDefaults[i]
		}
	}
	return o, nil
}

func narrowLimits(want, ceiling Limits) (Limits, error) {
	a := []*int{&want.Series, &want.Blocks, &want.PayloadBytes, &want.DecodedSamples, &want.OutputSamples}
	b := []int{ceiling.Series, ceiling.Blocks, ceiling.PayloadBytes, ceiling.DecodedSamples, ceiling.OutputSamples}
	for i, p := range a {
		if *p < 0 {
			return want, fmt.Errorf("%w: negative query limit", ErrInvalid)
		}
		if *p == 0 || *p > b[i] {
			*p = b[i]
		}
	}
	return want, nil
}
