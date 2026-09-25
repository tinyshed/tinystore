// Package admission lets work into an engine while it is open, a bounded
// number of each kind at a time, and lets Close wait for the work already in.
// It knows no engine: an engine names its own closed error.
package admission

import (
	"context"
	"sync"
)

// Gate counts the work inside an engine. Its zero value is open.
type Gate struct {
	mu      sync.Mutex
	active  int
	closing bool
	drained chan struct{}
}

// Enter lets one piece of work in, or refuses it with closed once Close was
// called; a caller that entered calls Leave once
func (g *Gate) Enter(ctx context.Context, closed error) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.closing {
		return closed
	}
	g.active++
	return nil
}

func (g *Gate) Leave() {
	g.mu.Lock()
	defer g.mu.Unlock()
	g.active--
	if g.closing && g.active == 0 {
		close(g.drainedChannel())
	}
}

// Close refuses new work and returns what closes when the work already in has
// left; first says this call was the one that closed the gate
func (g *Gate) Close() (drained <-chan struct{}, first bool) {
	g.mu.Lock()
	defer g.mu.Unlock()
	drainedChannel := g.drainedChannel()
	if g.closing {
		return drainedChannel, false
	}
	g.closing = true
	if g.active == 0 {
		close(drainedChannel)
	}
	return drainedChannel, true
}

// drainedChannel is made on first use, so that the zero Gate is ready; the
// caller holds mu
func (g *Gate) drainedChannel() chan struct{} {
	if g.drained == nil {
		g.drained = make(chan struct{})
	}
	return g.drained
}

// Slots bounds how many of one kind of work run at once; a caller waiting for
// one leaves the queue when its context ends
type Slots chan struct{}

func NewSlots(count int) Slots {
	return make(Slots, count)
}

// Take waits for a slot and returns what gives it back
func (s Slots) Take(ctx context.Context) (release func(), err error) {
	select {
	case s <- struct{}{}:
	case <-ctx.Done():
		return nil, ctx.Err()
	}
	if err = ctx.Err(); err != nil {
		<-s
		return nil, err
	}
	return sync.OnceFunc(func() { <-s }), nil
}
