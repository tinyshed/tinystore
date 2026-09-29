package kv

import (
	"bytes"
	"testing"
	"time"
)

// Maintain deletes expired keys with the values they spilled, and leaves the
// live ones
func TestMaintainDeletesExpiredKeysAndTheirSpilledValues(t *testing.T) {
	state := openTestState(t, t.TempDir())
	bucket := openTestBucket[[]byte](t, state, "cache", DefaultTTL(time.Minute))
	large := bytes.Repeat([]byte("v"), inlineLimit+1)
	for n := range 3 {
		if err := bucket.Set(t.Context(), n, large); err != nil {
			t.Fatal(err)
		}
	}
	if err := bucket.Set(t.Context(), "kept", []byte("small"), TTL(time.Hour)); err != nil {
		t.Fatal(err)
	}
	state.clock.advance(2 * time.Minute)

	done, err := state.Maintain(t.Context())
	if err != nil || done.Expired != 3 {
		t.Fatalf("Maintain expired %d, %v; want 3", done.Expired, err)
	}
	if spilled := state.spilledRows(t); spilled != 0 {
		t.Fatalf("%d spilled rows outlived their keys", spilled)
	}
	if _, found, _ := bucket.Get(t.Context(), "kept"); !found {
		t.Fatal("Maintain deleted a live key")
	}
}

// A Maintain that stops at its bound with expired or cleared rows left marks a
// backlog. The store's ten-second pass takes it rather than waiting for the
// next minute, and with nothing left that pass does nothing.
func TestAMaintainPastItsBoundIsFollowedSoon(t *testing.T) {
	state := openTestState(t, t.TempDir())
	state.expireBound, state.clearBound = 2, 2
	cache := openTestBucket[string](t, state, "cache", DefaultTTL(time.Minute))
	sessions := openTestBucket[string](t, state, "sessions")
	for n := range 25 {
		if err := cache.Set(t.Context(), n, "v"); err != nil {
			t.Fatal(err)
		}
		if err := sessions.Of(42).Set(t.Context(), n, "v"); err != nil {
			t.Fatal(err)
		}
	}
	if err := sessions.Of(42).Clear(t.Context()); err != nil {
		t.Fatal(err)
	}
	state.clock.advance(2 * time.Minute)

	done, err := state.Maintain(t.Context())
	if err != nil || done.Expired != 20 || done.Cleared != 20 || !state.backlog.Load() {
		t.Fatalf("a first pass expired %d and cleared %d, backlog %t, %v; want 20, 20 and a backlog",
			done.Expired, done.Cleared, state.backlog.Load(), err)
	}
	for range 2 {
		if err = state.catchUpInBackground(t.Context()); err != nil {
			t.Fatal(err)
		}
	}
	if rows := state.rowsOf(t, "cache") + state.rowsOf(t, "sessions"); rows != 0 || state.backlog.Load() {
		t.Fatalf("%d rows after the passes that follow, backlog %t", rows, state.backlog.Load())
	}
}
