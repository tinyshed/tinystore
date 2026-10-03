package server

import (
	"fmt"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/server/wire"
)

// Clock is the time a store runs on in a test, which an admin's server.clock
// sets or moves forward: tinystore serve --stdio --clock gives one to the
// private server an SDK starts for a test. A server without one runs on the
// system's time and refuses the call.
//
//	clock := server.NewClock(start)
//	store, err := tinystore.Open(ctx, dir, tinystore.Options{Clock: clock.Now})
//	srv, err := server.New(store, server.Options{Clock: clock})
type Clock struct {
	unixNano atomic.Int64
}

// NewClock is a clock that reads start until it is moved.
func NewClock(start time.Time) *Clock {
	c := &Clock{}
	c.unixNano.Store(start.UnixNano())
	return c
}

// Now is the clock's time, for tinystore.Options.Clock.
func (c *Clock) Now() time.Time {
	return time.Unix(0, c.unixNano.Load()).UTC()
}

// move sets the clock to at, which may not be before its time: a lease, a
// watermark and a key's expiry all count on time going one way
func (c *Clock) move(at time.Time) error {
	for {
		was := c.unixNano.Load()
		if at.UnixNano() < was {
			return fmt.Errorf("%w: server: a clock moves forward, and %s is before %s", tinystore.ErrInvalid,
				at.UTC().Format(time.RFC3339Nano), time.Unix(0, was).UTC().Format(time.RFC3339Nano))
		}
		if c.unixNano.CompareAndSwap(was, at.UnixNano()) {
			return nil
		}
	}
}

// serverClock moves the clock of a server started for a test, or reads it,
// for an admin's connection alone
func serverClock(c *call) error {
	var ask wire.Clock
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	clock := c.session.server.options.Clock
	switch {
	case c.session.capability != wire.Admin:
		return fmt.Errorf("%w: server: a clock", errAdminOnly)
	case clock == nil:
		return fmt.Errorf("%w: server: this server runs on the system's time; a private server started with a "+
			"clock moves", tinystore.ErrInvalid)
	case ask.At != 0 && ask.Advance != 0:
		return fmt.Errorf("%w: server: a clock is set to a time or moved by a while, not both", tinystore.ErrInvalid)
	}
	at := clock.Now().Add(durationOf(ask.Advance))
	if ask.At != 0 {
		at = time.UnixMilli(ask.At)
	}
	if err := clock.move(at); err != nil {
		return err
	}
	return respond(c, wire.Clock{At: clock.Now().UnixMilli()})
}
