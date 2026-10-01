package tinystore

import (
	"context"
	"fmt"
	"slices"
	"time"
)

// Measure is an integer counter or gauge reported by an engine. Names have
// bounded, fixed cardinality: do not put a bucket, queue, key or path in them.
type Measure struct {
	Engine, Name string
	Value        uint64
	Counter      bool
}

type Reporter interface {
	Report() []Measure
}

// SelfWriter writes one report without counting that write as user ingest.
// The root uses this interface without importing the metrics engine.
type SelfWriter interface {
	WriteSelf(context.Context, []Measure) error
}

const (
	selfInterval    = 15 * time.Second
	maxSelfMeasures = 256
	maxMeasureName  = 64
)

type selfMetrics struct {
	slot   chan struct{}
	writer SelfWriter // protected by Store.mu
	soon   func()
}

// FlushSelfMetrics captures available reports and the store's memory budget
// once, then commits their samples together. It is a no-op when disabled or
// when no metrics engine is open. It reports reserved bytes, not process RSS.
func (s *Store) FlushSelfMetrics(ctx context.Context) error {
	return s.flushSelf(ctx, false)
}

func (s *Store) flushSelf(ctx context.Context, closing bool) error {
	if s.self == nil {
		return nil
	}
	if closing {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, 5*time.Second)
		defer cancel()
	}
	select {
	case s.self.slot <- struct{}{}:
	case <-ctx.Done():
		return ctx.Err()
	}
	defer func() { <-s.self.slot }()

	writer, engines, err := s.selfSources(closing)
	if err != nil || writer == nil {
		return err
	}
	if err = ctx.Err(); err != nil {
		return err
	}
	measures, err := s.collectSelf(engines)
	if err != nil {
		return err
	}
	return writer.WriteSelf(ctx, measures)
}

func (s *Store) selfSources(closing bool) (SelfWriter, []Engine, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed && !closing {
		return nil, nil, ErrClosed
	}
	if s.self.writer == nil {
		return nil, nil, nil
	}
	if len(s.engines) > maxSelfMeasures {
		return nil, nil, fmt.Errorf("%w: self-metrics engines", ErrLimit)
	}
	return s.self.writer, slices.Clone(s.engines), nil
}

func (s *Store) collectSelf(engines []Engine) ([]Measure, error) {
	memory := s.Memory()
	used := uint64(memory.Used)         //nolint:gosec // budget usage is nonnegative
	peak := uint64(memory.Peak)         //nolint:gosec // budget usage is nonnegative
	capacity := uint64(memory.Capacity) //nolint:gosec // capacity is checked by Open
	measures := []Measure{
		{Engine: "store", Name: "memory_used_bytes", Value: used},
		{Engine: "store", Name: "memory_peak_bytes", Value: peak},
		{Engine: "store", Name: "memory_capacity_bytes", Value: capacity},
		{Engine: "store", Name: "engines", Value: uint64(len(engines))},
	}
	for _, engine := range engines {
		if reporter, ok := engine.(Reporter); ok {
			report := reporter.Report()
			if len(report) > maxSelfMeasures-len(measures) {
				return nil, fmt.Errorf("%w: self-metrics measurements", ErrLimit)
			}
			measures = append(measures, report...)
		}
	}
	if err := checkSelfMeasures(measures); err != nil {
		return nil, err
	}
	return measures, nil
}

func checkSelfMeasures(measures []Measure) error {
	seen := make(map[[2]string]bool, len(measures))
	for _, measure := range measures {
		key := [2]string{measure.Engine, measure.Name}
		if !measureName(measure.Engine) || !measureName(measure.Name) || seen[key] {
			return fmt.Errorf("%w: self-metrics %q/%q", ErrInvalid, measure.Engine, measure.Name)
		}
		if measure.Value > 1<<53 {
			return fmt.Errorf("%w: self-metrics %s/%s exceeds exact float64 integers", ErrLimit,
				measure.Engine, measure.Name)
		}
		seen[key] = true
	}
	return nil
}

func measureName(name string) bool {
	if name == "" || len(name) > maxMeasureName {
		return false
	}
	for i, c := range name {
		if c != '_' && (c < 'a' || c > 'z') && (i == 0 || c < '0' || c > '9') {
			return false
		}
	}
	return true
}
