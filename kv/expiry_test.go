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
