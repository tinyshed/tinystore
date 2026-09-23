package metrics

import (
	"context"
	"fmt"
	"math"
	"sync"
)

// WorkBudget shares active-work reservations across Store handles that use it.
type WorkBudget struct {
	mu                   sync.Mutex
	capacity, used, peak int64
	wake                 chan struct{}
}

func NewWorkBudget(bytes int64) (*WorkBudget, error) {
	if bytes <= 0 {
		return nil, fmt.Errorf("%w: shared work budget", ErrInvalid)
	}
	return &WorkBudget{capacity: bytes, wake: make(chan struct{})}, nil
}

func (b *WorkBudget) Usage() (used, peak int64) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.used, b.peak
}

func (b *WorkBudget) acquire(ctx context.Context, bytes int64) error {
	if bytes <= 0 || bytes > b.capacity {
		return fmt.Errorf("%w: shared work reservation", ErrLimit)
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	for bytes > b.capacity-b.used {
		wake := b.wake
		b.mu.Unlock()
		select {
		case <-wake:
		case <-ctx.Done():
			b.mu.Lock()
			return ctx.Err()
		}
		b.mu.Lock()
	}
	if err := ctx.Err(); err != nil {
		return err
	}
	b.used += bytes
	b.peak = max(b.peak, b.used)
	return nil
}

func (b *WorkBudget) release(bytes int64) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if bytes <= 0 || bytes > b.used {
		panic("metrics: invalid shared work release")
	}
	b.used -= bytes
	close(b.wake)
	b.wake = make(chan struct{})
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
