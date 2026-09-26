package kv

import (
	"errors"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// the term of a session that slides, and a handle on the same bucket that
// reads without renewing, to see the expiry the file holds
const slidingTerm = 30 * 24 * time.Hour

func openSlidingSessions(t *testing.T, state *testState) (sessions, plain *Bucket[string]) {
	t.Helper()
	return openTestBucket[string](t, state, "sessions", Sliding(slidingTerm)),
		openTestBucket[string](t, state, "sessions")
}

func (s *testState) expiryOf(t *testing.T, plain *Bucket[string], key string) time.Time {
	t.Helper()
	entry, found, err := plain.GetEntry(t.Context(), key)
	if err != nil || !found {
		t.Fatalf("%q: %v, found %v", key, err, found)
	}
	return entry.ExpiresAt
}

// reads renew a sliding key once a thirtieth of its term has passed since it
// last did, and a read writes nothing: the renewal waits for the flush
func TestASlidingReadWritesAtMostOncePerRefresh(t *testing.T) {
	state := openTestState(t, t.TempDir())
	sessions, plain := openSlidingSessions(t, state)
	ctx := t.Context()
	if err := sessions.Set(ctx, "token", "s"); err != nil {
		t.Fatal(err)
	}
	for range 3 {
		state.clock.advance(7 * time.Hour)
		if _, found, err := sessions.Get(ctx, "token"); !found || err != nil {
			t.Fatalf("a live session read %v, %v", found, err)
		}
	}
	if done, err := state.Maintain(ctx); done.Renewed != 0 || err != nil {
		t.Fatalf("reads within a thirtieth of the term renewed %d, %v", done.Renewed, err)
	}

	state.clock.advance(4 * time.Hour)
	for range 5 {
		if found, err := sessions.Has(ctx, "token"); !found || err != nil {
			t.Fatalf("a live session is not there: %v", err)
		}
	}
	if expires := state.expiryOf(t, plain, "token"); !expires.Equal(testStart.Add(slidingTerm)) {
		t.Fatalf("a read wrote its renewal before the flush: %v", expires)
	}
	done, err := state.Maintain(ctx)
	if err != nil || done.Renewed != 1 {
		t.Fatalf("five reads renewed %d times, %v; want once", done.Renewed, err)
	}
	if expires := state.expiryOf(t, plain, "token"); !expires.Equal(state.clock.Now().Add(slidingTerm)) {
		t.Fatalf("the renewed session expires at %v", expires)
	}
}

// a renewal is bound to the row its read saw: a key deleted and written again
// with the very expiry the read saw, or touched since, keeps what was done
func TestARenewalDoesNotExtendANewerIncarnation(t *testing.T) {
	state := openTestState(t, t.TempDir())
	sessions, plain := openSlidingSessions(t, state)
	ctx := t.Context()
	first, err := sessions.SetEntry(ctx, "rewritten", "first")
	if err != nil {
		t.Fatal(err)
	}
	if err = sessions.Set(ctx, "touched", "s"); err != nil {
		t.Fatal(err)
	}
	state.clock.advance(2 * 24 * time.Hour)
	for _, key := range []string{"rewritten", "touched"} {
		if _, found, getErr := sessions.Get(ctx, key); !found || getErr != nil {
			t.Fatalf("%s: %v", key, getErr)
		}
	}

	if err = sessions.Delete(ctx, "rewritten"); err != nil {
		t.Fatal(err)
	}
	if err = sessions.Set(ctx, "rewritten", "second", ExpireAt(first.ExpiresAt)); err != nil {
		t.Fatal(err)
	}
	if _, err = sessions.Touch(ctx, "touched", TTL(time.Hour)); err != nil {
		t.Fatal(err)
	}
	if done, maintainErr := state.Maintain(ctx); done.Renewed != 0 || maintainErr != nil {
		t.Fatalf("stale renewals renewed %d keys, %v", done.Renewed, maintainErr)
	}
	if expires := state.expiryOf(t, plain, "rewritten"); !expires.Equal(first.ExpiresAt) {
		t.Fatalf("a renewal of the first key extended the second to %v", expires)
	}
	if expires := state.expiryOf(t, plain, "touched"); !expires.Equal(state.clock.Now().Add(time.Hour)) {
		t.Fatalf("a renewal undid a Touch: %v", expires)
	}
}

// a key read in its last minute is renewed before its read returns, since the
// flush might come after it expired
func TestAReadNearItsExpiryRenewsAtOnce(t *testing.T) {
	state := openTestState(t, t.TempDir())
	sessions, plain := openSlidingSessions(t, state)
	if err := sessions.Set(t.Context(), "token", "s"); err != nil {
		t.Fatal(err)
	}
	state.clock.advance(slidingTerm - 30*time.Second)
	if _, found, err := sessions.Get(t.Context(), "token"); !found || err != nil {
		t.Fatalf("a session in its last minute: %v, %v", found, err)
	}
	if expires := state.expiryOf(t, plain, "token"); !expires.Equal(state.clock.Now().Add(slidingTerm)) {
		t.Fatalf("a read in the last minute left the expiry at %v", expires)
	}
}

// Sliding gives new keys their lifetime, so a DefaultTTL beside it says
// something else and is refused
func TestSlidingAndDefaultTTLAreOneOrTheOther(t *testing.T) {
	state := openTestState(t, t.TempDir())
	_, err := OpenBucket[string](t.Context(), state.Store, "sessions", Sliding(time.Hour), DefaultTTL(time.Minute))
	if !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("Sliding with DefaultTTL: %v", err)
	}
}
