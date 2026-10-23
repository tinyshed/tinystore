package core_test

// The link's rules for a lost connection, against a server that answers by a
// script, a connection of its own each time the link dials.

import (
	"context"
	"sync"
	"testing"

	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/scripted"
	"tinyshed.org/tinystore/internal/wire"
)

// contract: WIRE-05
func TestAWriteWhoseHandlesOpenLostTheConnectionGoesAgainOnTheNextSinceItNeverLeft(t *testing.T) {
	var mu sync.Mutex
	dialled, sets := 0, 0
	dial := func(ctx context.Context) (*core.Connection, error) {
		mu.Lock()
		dialled++
		first := dialled == 1
		mu.Unlock()
		return scripted.Dial(func(f scripted.Frame) ([]scripted.Frame, bool) {
			mu.Lock()
			defer mu.Unlock()
			if f.Method == wire.MethodKvBucketOpen {
				if first {
					return nil, true
				}
				return []scripted.Frame{scripted.Answer(f.Stream, wire.Encode(&wire.Handle{Handle: 7}))}, false
			}
			sets++
			written := wire.Encode(&wire.KvWritten{Written: true, Version: make([]byte, 8)})
			return []scripted.Frame{scripted.Answer(f.Stream, written)}, false
		})(ctx)
	}
	link := core.NewLink(dial)
	defer link.Close()
	opener := &core.Opener{Method: wire.MethodKvBucketOpen, Body: wire.Encode(&wire.KvBucketOpen{Name: "sessions"})}
	// what the script answers depends on no field of the write's body
	body := func(handle uint64) []byte { return wire.Encode(&wire.Handle{Handle: handle}) }
	if _, err := (core.ViaLink{Link: link}).Call(context.Background(), opener, wire.MethodKvSet, body, core.Writing); err != nil {
		t.Fatalf("the write: %v", err)
	}
	mu.Lock()
	defer mu.Unlock()
	if dialled != 2 || sets != 1 {
		t.Fatalf("%d connections dialled and %d writes made, not 2 and 1", dialled, sets)
	}
}
