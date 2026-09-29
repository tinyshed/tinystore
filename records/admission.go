package records

import (
	"context"
	"errors"
	"fmt"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/admission"
)

var errClosed = fmt.Errorf("records: %w", tinystore.ErrClosed)

func (s *Store) admit(ctx context.Context) (release func(), err error) {
	if err = s.gate.Enter(ctx, errClosed); err != nil {
		return nil, err
	}
	return s.gate.Leave, nil
}

// admitTo lets one operation in and holds one of slots through all of its
// work, decoding and encoding included. What runs at once is therefore bounded
// whether or not the store has Options.Memory.
func (s *Store) admitTo(ctx context.Context, slots admission.Slots) (release func(), err error) {
	leave, err := s.admit(ctx)
	if err != nil {
		return nil, err
	}
	free, err := slots.Take(ctx)
	if err != nil {
		leave()
		return nil, err
	}
	return func() {
		free()
		leave()
	}, nil
}

// reserve holds an operation's weight in the store's memory; a store without
// Options.Memory is not asked, so the weight is not even computed
func (s *Store) reserve(ctx context.Context, weigh func() int64) (release func(), err error) {
	if s.runtime == nil || s.runtime.Memory().Capacity == 0 {
		return func() {}, nil
	}
	reserved, err := s.runtime.Reserve(ctx, max(weigh(), 1))
	if errors.Is(err, tinystore.ErrLimit) {
		return nil, fmt.Errorf("records: %w", err)
	}
	if err != nil {
		return nil, err
	}
	return reserved.Release, nil
}

func (s *Store) holdMaintenance(ctx context.Context) (release func(), err error) {
	select {
	case <-s.maintenance:
		return func() { s.maintenance <- struct{}{} }, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}
