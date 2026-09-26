package kv

import (
	"errors"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// sessions of two users, one with a branch of devices under it, and a user
// whose name begins with the first one's bytes and a zero
func writeSessions(t *testing.T, sessions *Bucket[string]) {
	t.Helper()
	for _, write := range []struct {
		bucket *Bucket[string]
		key    string
	}{
		{sessions.Of(42), "a"},
		{sessions.Of(42), "b"},
		{sessions.Of(42), "c"},
		{sessions.Of(42, "devices"), "phone"},
		{sessions.Of(43), "d"},
		{sessions.Of("42\x00"), "e"},
	} {
		if err := write.bucket.Set(t.Context(), write.key, write.key); err != nil {
			t.Fatal(err)
		}
	}
}

// a Clear empties its branch and the branches under it and nothing beside
// them, whether it deletes the keys at once or marks the branch and leaves
// the rows to maintenance
func TestClearEmptiesTheBranchAndThoseUnderIt(t *testing.T) {
	for _, bound := range []int{clearAtOnce, 2} {
		t.Run(map[int]string{clearAtOnce: "at once", 2: "marked"}[bound], func(t *testing.T) {
			state := openTestState(t, t.TempDir())
			state.clearBound = bound
			sessions := openTestBucket[string](t, state, "sessions")
			writeSessions(t, sessions)
			if err := sessions.Of(42).Clear(t.Context()); err != nil {
				t.Fatal(err)
			}

			for _, gone := range []struct {
				bucket *Bucket[string]
				key    string
			}{{sessions.Of(42), "a"}, {sessions.Of(42), "c"}, {sessions.Of(42, "devices"), "phone"}} {
				if found, err := gone.bucket.Has(t.Context(), gone.key); found || err != nil {
					t.Fatalf("%q is there after the Clear: %v", gone.key, err)
				}
			}
			if page, err := sessions.Of(42).Scan(t.Context(), Query{}); len(page.Entries) != 0 || err != nil {
				t.Fatalf("a scan of the cleared branch found %d keys, %v", len(page.Entries), err)
			}
			for _, kept := range []*Bucket[string]{sessions.Of(43), sessions.Of("42\x00")} {
				if page, _ := kept.Scan(t.Context(), Query{}); len(page.Entries) != 1 {
					t.Fatalf("a branch beside the cleared one holds %d keys", len(page.Entries))
				}
			}

			if err := sessions.Of(42).Set(t.Context(), "a", "again"); err != nil {
				t.Fatal(err)
			}
			done, err := state.Maintain(t.Context())
			if err != nil || state.rowsOf(t, "sessions") != 3 {
				t.Fatalf("after maintenance %d rows, %d cleared, %v; want the two beside and the one written again",
					state.rowsOf(t, "sessions"), done.Cleared, err)
			}
			if value, _, _ := sessions.Of(42).Get(t.Context(), "a"); value != "again" {
				t.Fatalf("a key written after the Clear reads %q", value)
			}
		})
	}
}

// a key a mark hid is absent to every call, as an expired one is: its old
// version conflicts, and SetIfAbsent claims it
func TestAClearedKeyIsAbsentToEveryOperation(t *testing.T) {
	state := openTestState(t, t.TempDir())
	state.clearBound = 0
	drafts := openTestBucket[string](t, state, "drafts")
	old, err := drafts.Of(7).SetEntry(t.Context(), "note", "old")
	if err != nil {
		t.Fatal(err)
	}
	if err = drafts.Clear(t.Context()); err != nil {
		t.Fatal(err)
	}

	if _, found, takeErr := drafts.Of(7).Take(t.Context(), "note"); found || takeErr != nil {
		t.Fatalf("Take found a cleared key: %v", takeErr)
	}
	if found, touchErr := drafts.Of(7).Touch(t.Context(), "note", TTL(time.Hour)); found || touchErr != nil {
		t.Fatalf("Touch found a cleared key: %v", touchErr)
	}
	if err = drafts.Of(7).Set(t.Context(), "note", "new", IfVersion(old.Version)); !errors.Is(err,
		tinystore.ErrConflict) {
		t.Fatalf("IfVersion passed on a cleared key: %v", err)
	}
	created, err := drafts.Of(7).SetIfAbsent(t.Context(), "note", "claimed")
	if err != nil || !created {
		t.Fatalf("SetIfAbsent did not claim a cleared key: %v", err)
	}
}

// a Clear of LoseAtMost counters drops what memory holds under the branch in
// its own transaction, so no flush after it writes those counters again
func TestAClearDoesNotResurrectCountersWaitingForTheFlush(t *testing.T) {
	for _, bound := range []int{clearAtOnce, 0} {
		state := openTestState(t, t.TempDir())
		state.clearBound = bound
		attempts := openTestCounters(t, state, "attempts", LoseAtMost(time.Hour))
		ctx := t.Context()
		for _, key := range []string{"flushed", "waiting"} {
			if _, err := attempts.Of("ip").Add(ctx, key, 37); err != nil {
				t.Fatal(err)
			}
			if _, err := state.Maintain(ctx); err != nil && key == "flushed" {
				t.Fatal(err)
			}
		}
		if _, err := attempts.Of("ip").Add(ctx, "waiting", 1); err != nil {
			t.Fatal(err)
		}
		if _, err := attempts.Of("email").Add(ctx, "kept", 5); err != nil {
			t.Fatal(err)
		}

		if err := attempts.Of("ip").Clear(ctx); err != nil {
			t.Fatal(err)
		}
		if _, err := state.Maintain(ctx); err != nil {
			t.Fatal(err)
		}
		reopened := state.reopen(t)
		attempts = openTestCounters(t, reopened, "attempts", LoseAtMost(time.Hour))
		for key, want := range map[string]int64{"flushed": 0, "waiting": 0} {
			if held, err := attempts.Of("ip").Get(ctx, key); held != want || err != nil {
				t.Fatalf("bound %d: %s holds %d after the Clear, %v", bound, key, held, err)
			}
		}
		if held, _ := attempts.Of("email").Get(ctx, "kept"); held != 5 {
			t.Fatalf("bound %d: a counter beside the cleared branch holds %d", bound, held)
		}
	}
}

// inside a transaction a Clear deletes what it clears or refuses: a mark
// would leave the deleting to maintenance after the caller asked for it
func TestAClearInATransactionOverTheBoundIsRefused(t *testing.T) {
	state := openTestState(t, t.TempDir())
	state.clearBound = 2
	sessions := openTestBucket[string](t, state, "sessions")
	writeSessions(t, sessions)
	err := state.Tx(t.Context(), func(tx *Tx) error {
		if clearErr := sessions.Of(43).WithTx(tx).Clear(t.Context()); clearErr != nil {
			t.Fatalf("a Clear of one key inside a transaction: %v", clearErr)
		}
		return sessions.Of(42).WithTx(tx).Clear(t.Context())
	})
	if !errors.Is(err, tinystore.ErrLimit) {
		t.Fatalf("a Clear of four keys inside a transaction bound at two: %v", err)
	}
	if found, _ := sessions.Of(43).Has(t.Context(), "d"); !found {
		t.Fatal("the refused transaction's first Clear was kept")
	}
}

// durable counters under a cleared branch start again from zero
func TestClearedCountersStartFromZero(t *testing.T) {
	state := openTestState(t, t.TempDir())
	state.clearBound = 0
	hits := openTestCounters(t, state, "hits")
	if _, err := hits.Of("page").Add(t.Context(), "home", 9); err != nil {
		t.Fatal(err)
	}
	if err := hits.Clear(t.Context()); err != nil {
		t.Fatal(err)
	}
	if n, err := hits.Of("page").Add(t.Context(), "home", 1); n != 1 || err != nil {
		t.Fatalf("an Add after the Clear gave %d, %v", n, err)
	}
}
