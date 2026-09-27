package kv

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"slices"
	"sync"
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

// what makes a Clear fail: a statement it runs, or its commit
const (
	refuseClears = `create trigger refuse_delete before delete on cells
			begin select raise(abort, 'a refused Clear'); end;
		create trigger refuse_mark before insert on branches
			begin select raise(abort, 'a refused Clear'); end`
	refuseCommits = `create table refused_parent (id integer primary key);
		create table refused_child (parent integer references refused_parent (id) deferrable initially deferred);
		create trigger refuse_delete after delete on cells begin insert into refused_child values (1); end;
		create trigger refuse_mark after insert on branches begin insert into refused_child values (1); end`
	acceptAgain = `drop trigger refuse_delete; drop trigger refuse_mark;
		drop table if exists refused_child; drop table if exists refused_parent`
)

func (s *testState) exec(t *testing.T, statements string) {
	t.Helper()
	err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), statements)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
}

// attemptsWaiting is a counter the file holds at 7 and memory at 12, and one
// beside it that only memory holds
func attemptsWaiting(t *testing.T, state *testState) *Counters {
	t.Helper()
	attempts := openTestCounters(t, state, "attempts", LoseAtMost(time.Hour))
	if _, err := attempts.Of("ip").Add(t.Context(), "key", 7); err != nil {
		t.Fatal(err)
	}
	if _, err := state.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	if _, err := attempts.Of("ip").Add(t.Context(), "key", 5); err != nil {
		t.Fatal(err)
	}
	if _, err := attempts.Of("email").Add(t.Context(), "kept", 3); err != nil {
		t.Fatal(err)
	}
	return attempts
}

func (s *testState) expectAttempts(t *testing.T, attempts *Counters, key string, kept int64) {
	t.Helper()
	if held, err := attempts.Of("ip").Get(t.Context(), "key"); held != kept || err != nil {
		t.Fatalf("%s: the counter under the branch holds %d, %v; want %d", key, held, err, kept)
	}
	if held, err := attempts.Of("email").Get(t.Context(), "kept"); held != 3 || err != nil {
		t.Fatalf("%s: the counter beside the branch holds %d, %v", key, held, err)
	}
}

// a Clear of LoseAtMost counters that rolls back leaves memory as it was, a
// change waiting for the flush included, whether it deletes or marks
func TestAFailedClearKeepsTheCountersWaitingForTheFlush(t *testing.T) {
	for _, bound := range []int{clearAtOnce, 0} {
		state := openTestState(t, t.TempDir())
		state.clearBound = bound
		attempts := attemptsWaiting(t, state)

		cancelled, cancel := context.WithCancel(t.Context())
		cancel()
		if err := attempts.Of("ip").Clear(cancelled); !errors.Is(err, context.Canceled) {
			t.Fatalf("bound %d: a Clear whose caller left: %v", bound, err)
		}
		state.exec(t, refuseClears)
		err := attempts.Of("ip").Clear(t.Context())
		state.exec(t, acceptAgain)
		if err == nil || errors.Is(err, ErrOutcomeUnknown) {
			t.Fatalf("bound %d: a refused Clear: %v", bound, err)
		}
		state.expectAttempts(t, attempts, "after the refused Clear", 12)

		if _, err = state.Maintain(t.Context()); err != nil {
			t.Fatal(err)
		}
		reopened := state.reopen(t)
		reopened.expectAttempts(t, openTestCounters(t, reopened, "attempts", LoseAtMost(time.Hour)), "reopened", 12)
	}
}

// a Clear whose commit fails may be in the file, so memory lets go of the
// branch as a crash would: its reads go to the file, and nothing beside the
// branch is lost
func TestAClearWhoseCommitFailsLetsGoAsACrashWould(t *testing.T) {
	for _, bound := range []int{clearAtOnce, 0} {
		state := openTestState(t, t.TempDir())
		state.clearBound = bound
		attempts := attemptsWaiting(t, state)

		state.exec(t, refuseCommits)
		err := attempts.Of("ip").Clear(t.Context())
		state.exec(t, acceptAgain)
		if !errors.Is(err, ErrOutcomeUnknown) {
			t.Fatalf("bound %d: a Clear whose commit failed: %v", bound, err)
		}
		state.expectAttempts(t, attempts, "after the failed commit", 7)

		if _, err = state.Maintain(t.Context()); err != nil {
			t.Fatal(err)
		}
		reopened := state.reopen(t)
		reopened.expectAttempts(t, openTestCounters(t, reopened, "attempts", LoseAtMost(time.Hour)), "reopened", 7)
	}
}

// Clears beside changes and flushes of counters in memory take nothing from
// the branches beside them, and leave nothing of their own branch behind
func TestClearsBesideChangesAndFlushesKeepTheirBranchesApart(t *testing.T) {
	for _, bound := range []int{clearAtOnce, 0} {
		state := openTestState(t, t.TempDir())
		state.clearBound = bound
		attempts := openTestCounters(t, state, "attempts", LoseAtMost(time.Hour))
		ctx := t.Context()
		var work sync.WaitGroup
		for worker := range 10 {
			branch := "email"
			if worker < 2 {
				branch = "ip"
			}
			work.Go(func() {
				for n := range 300 {
					if _, err := attempts.Of(branch).Add(ctx, n%5, 1); err != nil {
						t.Error(err)
						return
					}
				}
			})
		}
		work.Go(func() {
			for range 20 {
				if _, err := state.Maintain(ctx); err != nil {
					t.Error(err)
				}
				if err := attempts.Of("ip").Clear(ctx); err != nil {
					t.Error(err)
				}
			}
		})
		work.Wait()

		if err := attempts.Of("ip").Clear(ctx); err != nil {
			t.Fatal(err)
		}
		if _, err := state.Maintain(ctx); err != nil {
			t.Fatal(err)
		}
		reopened := state.reopen(t)
		attempts = openTestCounters(t, reopened, "attempts", LoseAtMost(time.Hour))
		total := int64(0)
		for key := range 5 {
			if held, _ := attempts.Of("ip").Get(ctx, key); held != 0 {
				t.Fatalf("bound %d: a cleared counter came back holding %d", bound, held)
			}
			held, err := attempts.Of("email").Get(ctx, key)
			if err != nil {
				t.Fatal(err)
			}
			total += held
		}
		if total != 8*300 {
			t.Fatalf("bound %d: %d Adds beside the cleared branch counted %d", bound, 8*300, total)
		}
	}
}

// a mark hides what lies under its branch at every depth, the branches the
// statements look up by length and those deeper than their slots alike, and
// nothing beside it or above it
func TestAMarkHidesWhatLiesUnderItAtEveryDepth(t *testing.T) {
	for _, depth := range []int{1, hiddenLevels, hiddenLevels + 1, hiddenLevels + 3} {
		t.Run(fmt.Sprintf("depth=%d", depth), func(t *testing.T) {
			state := openTestState(t, t.TempDir())
			state.clearBound = 0
			drafts := openTestBucket[string](t, state, "drafts")
			owners := make([]any, depth)
			for i := range owners {
				owners[i] = fmt.Sprintf("o%d", i)
			}
			cleared, above := drafts.Of(owners...), drafts.Of(owners[:depth-1]...)
			beside := above.Of("other")
			for _, branch := range []*Bucket[string]{cleared, cleared.Of("under"), above, beside} {
				if err := branch.Set(t.Context(), "note", "kept"); err != nil {
					t.Fatal(err)
				}
			}

			if err := cleared.Clear(t.Context()); err != nil {
				t.Fatal(err)
			}
			expectNotes(t, map[*Bucket[string]]bool{
				cleared: false, cleared.Of("under"): false, above: true,
				beside: true,
			})
			if created, err := cleared.SetIfAbsent(t.Context(), "note", "again"); !created || err != nil {
				t.Fatalf("SetIfAbsent under the mark: %v, %v", created, err)
			}

			if err := drafts.Clear(t.Context()); err != nil {
				t.Fatal(err)
			}
			expectNotes(t, map[*Bucket[string]]bool{cleared: false, above: false, beside: false})
		})
	}
}

// expectNotes checks each branch's "note" through a Get, a Has and a Scan
func expectNotes(t *testing.T, there map[*Bucket[string]]bool) {
	t.Helper()
	for branch, want := range there {
		_, found, err := branch.Get(t.Context(), "note")
		has, hasErr := branch.Has(t.Context(), "note")
		page, scanErr := branch.Scan(t.Context(), Query{})
		scanned := slices.ContainsFunc(page.Entries, func(entry Entry[string]) bool { return entry.Key == "note" })
		if found != want || has != want || scanned != want || errors.Join(err, hasErr, scanErr) != nil {
			t.Fatalf("%v: Get %v, Has %v, Scan %v, want %v: %v", branch.owners, found, has, scanned, want,
				errors.Join(err, hasErr, scanErr))
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
