package core

import (
	"context"
	"sync/atomic"
	"testing"
)

// A follower's stop ends its stream, and the follower then forgets its stop
// while the store's close still runs the others: Close must not read the
// stops while they are taken off. The race detector tells it at once; the
// runtime may abort on a map read while it is written.
func TestAStoresCloseRunsEveryStopWhileTheStopsThatEndedForgetThemselves(t *testing.T) {
	s := NewStore(NewLink(func(context.Context) (*Connection, error) {
		return nil, Errorf(Closed, "no server")
	}))
	const followers = 2000
	var stopped atomic.Int64
	for range followers {
		var forget func()
		forget = s.OnClose(StopFollowers, func() {
			stopped.Add(1)
			forget()
		})
	}
	s.Close()
	if n := stopped.Load(); n != followers {
		t.Fatalf("%d of %d stops ran", n, followers)
	}
}
