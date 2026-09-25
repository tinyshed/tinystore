package records

import (
	"context"
	"errors"
	"fmt"
	"sync"

	"github.com/tinyshed/tinystore"
)

// gate lets work in while the store is open, and lets Close wait for the work
// already in
type gate struct {
	mu      sync.Mutex
	active  int
	closing bool
	drained chan struct{}
}

func (g *gate) enter(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.closing {
		return fmt.Errorf("records: %w", tinystore.ErrClosed)
	}
	g.active++
	return nil
}

func (g *gate) leave() {
	g.mu.Lock()
	defer g.mu.Unlock()
	g.active--
	if g.closing && g.active == 0 {
		close(g.drained)
	}
}

// close refuses new work; drained closes when the work already in has left
func (g *gate) close() (drained <-chan struct{}) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if !g.closing {
		g.closing = true
		if g.active == 0 {
			close(g.drained)
		}
	}
	return g.drained
}

// admit lets one operation in while the store is open; release lets it out
func (s *Store) admit(ctx context.Context) (release func(), err error) {
	if err = s.gate.enter(ctx); err != nil {
		return nil, err
	}
	return s.gate.leave, nil
}

// reserve holds an operation's weight in the store's memory; a store without
// Options.Memory is not asked, so the weight is not even computed
func (s *Store) reserve(ctx context.Context, weigh func() int64) (release func(), err error) {
	if s.runtime == nil || s.runtime.Memory().Capacity == 0 {
		return func() {}, nil
	}
	release, err = s.runtime.Reserve(ctx, max(weigh(), 1))
	if errors.Is(err, tinystore.ErrLimit) {
		return nil, fmt.Errorf("records: %w", err)
	}
	return release, err
}

// holdMaintenance lets one Maintain at a time run on a store
func (s *Store) holdMaintenance(ctx context.Context) (release func(), err error) {
	select {
	case <-s.maintenance:
		return func() { s.maintenance <- struct{}{} }, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}
