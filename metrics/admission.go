package metrics

import "context"

// admit lets one operation in: the store is still open and one of the given
// slots is free. release gives both back.
func (s *Store) admit(ctx context.Context, slots chan struct{}) (release func(), err error) {
	if err := s.enter(ctx); err != nil {
		return nil, err
	}

	select {
	case slots <- struct{}{}:
	case <-ctx.Done():
		s.leave()
		return nil, ctx.Err()
	}
	release = func() {
		<-slots
		s.leave()
	}

	if err := ctx.Err(); err != nil {
		release()
		return nil, err
	}
	return release, nil
}

// reserve holds an operation's weight in the budget shared by several stores.
// A store opened without one holds nothing and never calls weigh.
func (s *Store) reserve(ctx context.Context, weigh func() (int64, error)) (release func(), err error) {
	budget := s.opts.SharedBudget
	if budget == nil {
		return func() {}, nil
	}

	weight, err := weigh()
	if err != nil {
		return nil, err
	}
	if err := budget.acquire(ctx, weight); err != nil {
		return nil, err
	}
	return func() { budget.release(weight) }, nil
}
