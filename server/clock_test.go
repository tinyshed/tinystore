package server

import (
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/server/wire"
)

// a test's clock moves only forward, at an admin's request, and the store runs
// on it: a key whose time has passed is gone without a wait
func TestATestsClockMovesOnlyForwardAndTheStoreRunsOnIt(t *testing.T) {
	start := time.Date(2026, 10, 3, 9, 0, 0, 0, time.UTC)
	clock := NewClock(start)
	root := t.TempDir()
	store, err := tinystore.Open(t.Context(), root, tinystore.Options{Manual: true, Clock: clock.Now})
	if err != nil {
		t.Fatal(err)
	}
	ts := serveTestStore(t, root, store, Options{Clock: clock})
	conn := ts.dial(t, wire.Hello{})
	codes := openKV(t, conn, wire.KVBucket{Name: "codes"})
	mustKV(t, conn, wire.KVSet, wire.KVCall{Handle: codes, Key: "K7Q2", Value: text("42"), TTL: time.Hour.Milliseconds()})

	read, err := conn.Call(t.Context(), wire.ServerClock, wire.Clock{})
	var now wire.Clock
	if err != nil || now.Decode(read) != nil || now.At != start.UnixMilli() {
		t.Fatalf("the clock reads %d: %v", now.At, err)
	}
	moved, err := conn.Call(t.Context(), wire.ServerClock, wire.Clock{Advance: (2 * time.Hour).Milliseconds()})
	if err != nil || now.Decode(moved) != nil || now.At != start.Add(2*time.Hour).UnixMilli() {
		t.Fatalf("the clock moved to %d: %v", now.At, err)
	}
	if got := mustKV(t, conn, wire.KVGet, wire.KVCall{Handle: codes, Key: "K7Q2"}); got.Found {
		t.Fatalf("a key an hour past its expiry: %+v", got)
	}
	set, err := conn.Call(t.Context(), wire.ServerClock, wire.Clock{At: start.Add(3 * time.Hour).UnixMilli()})
	if err != nil || now.Decode(set) != nil || !clock.Now().Equal(start.Add(3*time.Hour)) {
		t.Fatalf("the clock set to %v: %v", clock.Now(), err)
	}

	for _, refused := range []wire.Clock{
		{At: start.UnixMilli()},
		{Advance: -1},
		{At: start.Add(4 * time.Hour).UnixMilli(), Advance: 1},
	} {
		if _, err = conn.Call(t.Context(), wire.ServerClock, refused); failureOf(err).Code != wire.CodeInvalid {
			t.Errorf("a clock moved by %+v: %v", refused, err)
		}
	}
	plain := startTestServer(t, Options{})
	if _, err = plain.dial(t, wire.Hello{}).Call(t.Context(), wire.ServerClock, wire.Clock{}); failureOf(err).Code !=
		wire.CodeInvalid {
		t.Errorf("a clock of a server on the system's time: %v", err)
	}
}
