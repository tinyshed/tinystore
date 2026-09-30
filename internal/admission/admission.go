// Package admission lets work into an engine while it is open, a bounded
// number of each kind at a time, and lets Close wait for the work already in.
// It knows no engine: an engine names its own closed error.
package admission

import (
	"context"
	"sync"
	"sync/atomic"
)

// Gate counts the work inside an engine. Its zero value is open.
type Gate struct {
	state    atomic.Int64 // the closing bit, and below it the work inside
	making   sync.Mutex   // makes drained, which the zero Gate lacks
	drained  chan struct{}
	finished sync.Once
}

// the bit of a gate's state that says Close was called
const closing = 1 << 62

// Enter lets one piece of work in, or refuses it with closed once Close was
// called; a caller that entered calls Leave once. It counts itself in before
// it looks, so that entering is one atomic add and never a lock that every
// call of the engine would queue on.
func (g *Gate) Enter(ctx context.Context, closed error) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if g.state.Add(1)&closing != 0 {
		g.Leave()
		return closed
	}
	return nil
}

func (g *Gate) Leave() {
	if g.state.Add(-1) == closing {
		g.finish()
	}
}

// Close refuses new work and returns what closes when the work already in has
// left; first says this call was the one that closed the gate
func (g *Gate) Close() (drained <-chan struct{}, first bool) {
	g.making.Lock()
	if g.drained == nil {
		g.drained = make(chan struct{})
	}
	g.making.Unlock()

	before := g.state.Or(closing)
	if before == 0 {
		g.finish()
	}
	return g.drained, before&closing == 0
}

// finish closes drained once the last work has left a closing gate; a refused
// Enter counts itself in and out too, so that more than one may find it empty
func (g *Gate) finish() {
	g.finished.Do(func() { close(g.drained) })
}

// Slots bounds how many of one kind of work run at once; a caller waiting for
// one leaves the queue when its context ends
type Slots chan struct{}

func NewSlots(count int) Slots {
	return make(Slots, count)
}

// Take waits for a slot and returns what gives it back
func (s Slots) Take(ctx context.Context) (release func(), err error) {
	if err = ctx.Err(); err != nil {
		return nil, err
	}

	select {
	case s <- struct{}{}:
	default:
		select {
		case s <- struct{}{}:
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
	if err = ctx.Err(); err != nil {
		<-s
		return nil, err
	}
	return sync.OnceFunc(func() { <-s }), nil
}
