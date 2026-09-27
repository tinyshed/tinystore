package metrics

import (
	"context"
	"errors"
	"fmt"
	"math"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/admission"
)

// admit lets one operation in: the store is still open and one of the given
// slots is free. release gives both back.
func (s *Store) admit(ctx context.Context, slots admission.Slots) (release func(), err error) {
	if err = s.enter(ctx); err != nil {
		return nil, err
	}

	free, err := slots.Take(ctx)
	if err != nil {
		s.leave()
		return nil, err
	}
	return func() {
		free()
		s.leave()
	}, nil
}

// reserve holds an operation's weight in the store's memory; a store without
// Options.Memory is not asked, so the weight is not even computed
func (s *Store) reserve(ctx context.Context, weigh func() (int64, error)) (release func(), err error) {
	if s.runtime == nil || s.runtime.Memory().Capacity == 0 {
		return func() {}, nil
	}

	weight, err := weigh()
	if err != nil {
		return nil, err
	}
	reserved, err := s.runtime.Reserve(ctx, weight)
	if errors.Is(err, tinystore.ErrLimit) {
		return nil, fmt.Errorf("%w: %s", ErrLimit, err.Error())
	}
	if err != nil {
		return nil, err
	}
	return reserved.Release, nil
}

func (s *Store) enter(ctx context.Context) error {
	return s.gate.Enter(ctx, ErrClosed)
}

func (s *Store) leave() {
	s.gate.Leave()
}

func reservation(parts ...int64) (int64, error) {
	total := int64(0)
	for _, part := range parts {
		if part < 0 || part > math.MaxInt64-total {
			return 0, fmt.Errorf("%w: shared work reservation overflow", ErrLimit)
		}
		total += part
	}
	return max(total, 1), nil
}

func reservedMultiple(value int, factor int64) (int64, error) {
	if value < 0 || int64(value) > math.MaxInt64/factor {
		return 0, fmt.Errorf("%w: shared work reservation overflow", ErrLimit)
	}
	return int64(value) * factor, nil
}

func readReservation(limits Limits) (int64, error) {
	decoded, err := reservedMultiple(limits.DecodedSamples, 16)
	if err != nil {
		return 0, err
	}
	output, err := reservedMultiple(limits.OutputSamples, 16)
	if err != nil {
		return 0, err
	}
	series, err := reservedMultiple(limits.Series, 256)
	if err != nil {
		return 0, err
	}
	return reservation(int64(limits.PayloadBytes), decoded, output, series)
}

func (s *Store) ingestReservation(batches []Batch) (int64, error) {
	var input int64
	for _, batch := range batches {
		samples, err := reservedMultiple(len(batch.Samples), 64)
		if err != nil {
			return 0, err
		}
		labels, err := reservedMultiple(len(batch.Series.Labels), 64)
		if err != nil {
			return 0, err
		}
		input, err = reservation(input, samples, labels)
		if err != nil {
			return 0, err
		}
		for _, label := range batch.Series.Labels {
			name, sizeErr := reservedMultiple(len(label.Name), 4)
			if sizeErr != nil {
				return 0, sizeErr
			}
			value, sizeErr := reservedMultiple(len(label.Value), 4)
			if sizeErr != nil {
				return 0, sizeErr
			}
			input, err = reservation(input, name, value)
			if err != nil {
				return 0, err
			}
		}
	}
	return reservation(input, int64(s.opts.MaxHeadBytes)*2, 512<<10)
}

func (s *Store) maintenanceReservation() (int64, error) {
	points, err := reservedMultiple(s.opts.MaxHeadSamples, 16*publicationBatchSeries)
	if err != nil {
		return 0, err
	}
	return reservation(int64(s.opts.MaxHeadBytes)*2, publicationBatchBytes, points, 512<<10)
}
