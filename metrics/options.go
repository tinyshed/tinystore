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
	MaintenanceInterval time.Duration
	Limits              Limits
}

func normalizeOptions(o Options) (Options, error) {
	if err := o.normalizeDurations(); err != nil {
		return o, err
	}
	if err := o.normalizeCapacities(); err != nil {
		return o, err
	}
	if err := o.normalizeConcurrency(); err != nil {
		return o, err
	}
	if err := o.normalizeHead(); err != nil {
		return o, err
	}
	return o, o.Limits.normalize()
}

func (o *Options) normalizeDurations() error {
	if o.Retention == 0 {
		o.Retention = 30 * 24 * time.Hour
	}
	if o.SnapshotTimeout == 0 {
		o.SnapshotTimeout = 5 * time.Second
	}
	if o.MaintenanceInterval == 0 {
		o.MaintenanceInterval = time.Minute
	}
	if o.MaxBlockSpan == 0 {
		o.MaxBlockSpan = 24 * time.Hour
	}
	if o.MaxBlockSpan <= 0 || !wholeMilliseconds(o.MaxBlockSpan) {
		return fmt.Errorf("%w: block span", ErrInvalid)
	}
	if o.Retention <= 0 || !wholeMilliseconds(o.Retention) || !wholeMilliseconds(o.Lateness) {
		return fmt.Errorf("%w: durations", ErrInvalid)
	}
	if o.SnapshotTimeout < 0 || o.MaintenanceInterval < 0 {
		return fmt.Errorf("%w: durations", ErrInvalid)
	}
	return nil
}

func wholeMilliseconds(duration time.Duration) bool {
	return duration >= 0 && duration%time.Millisecond == 0
}

func (o *Options) normalizeCapacities() error {
	capacities := []struct {
		value    *int
		fallback int
	}{
		{&o.MaxSeries, 100000},
		{&o.MaxHeadSamples, 4096},
		{&o.MaxBatchSamples, 10000},
		{&o.MaxBatchBytes, 4 << 20},
		{&o.MaintenanceSeries, 64},
		{&o.MaxReaders, 2},
	}
	for _, capacity := range capacities {
		if *capacity.value < 0 {
			return fmt.Errorf("%w: negative capacity", ErrInvalid)
		}
		if *capacity.value == 0 {
			*capacity.value = capacity.fallback
		}
	}
	return nil
}

func (o *Options) normalizeConcurrency() error {
	for _, slots := range []int{o.MaxConcurrentReads, o.MaxConcurrentIngest} {
		if slots < 0 || slots > 1<<16 {
			return fmt.Errorf("%w: concurrent work capacity", ErrInvalid)
		}
	}
	if o.MaxConcurrentReads == 0 {
		o.MaxConcurrentReads = o.MaxReaders
	}
	if o.MaxConcurrentIngest == 0 {
		o.MaxConcurrentIngest = 1
	}
	return nil
}

func (o *Options) normalizeHead() error {
	if o.MaxHeadBytes == 0 {
		o.MaxHeadBytes = 256 << 10
	}
	if o.MaxHeadBytes < 1 || o.MaxHeadBytes > maximumHeadBytes || o.MaxHeadSamples > 1<<20 {
		return fmt.Errorf("%w: mutable head capacity", ErrInvalid)
	}
	return nil
}

// fields lists the limits in one order for every loop over them
func (l *Limits) fields() [5]*int {
	return [5]*int{&l.Series, &l.Blocks, &l.PayloadBytes, &l.DecodedSamples, &l.OutputSamples}
}

func (l *Limits) normalize() error {
	defaults := Limits{
		Series: 1000, Blocks: 4096, PayloadBytes: 16 << 20, DecodedSamples: 1 << 20, OutputSamples: 100000,
	}
	fallbacks := defaults.fields()
	for i, limit := range l.fields() {
		if *limit < 0 || *limit == math.MaxInt {
			return fmt.Errorf("%w: query capacity", ErrInvalid)
		}
		if *limit == 0 {
			*limit = *fallbacks[i]
		}
	}
	return nil
}

// narrowLimits takes a query's limits, each capped by the store's; zero asks for the store's:
//
//	store 10 20 30 40 50    query 5 0 31 0 0    → 5 20 30 40 50
func narrowLimits(want, ceiling Limits) (Limits, error) {
	caps := ceiling.fields()
	for i, limit := range want.fields() {
		if *limit < 0 {
			return want, fmt.Errorf("%w: negative query limit", ErrInvalid)
		}
		if *limit == 0 || *limit > *caps[i] {
			*limit = *caps[i]
		}
	}
	return want, nil
}
