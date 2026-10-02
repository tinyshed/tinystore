package kv

import (
	"errors"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

func openTestQuota(t *testing.T, state *testState, name string, windows ...QuotaOption) *Quota {
	t.Helper()
	quota, err := OpenQuota(t.Context(), state.Store, name, windows...)
	if err != nil {
		t.Fatal(err)
	}
	return quota
}

// expectUsage checks an answer: whether it passed, how many more would, and
// each window's use, in "name used/limit" order of the quota's windows
func expectUsage(t *testing.T, got QuotaUsage, err error, ok bool, left int64, windows map[string]int64) {
	t.Helper()
	if err != nil || got.OK != ok || got.Left != left {
		t.Fatalf("%+v, %v; want OK %v with %d left", got, err, ok, left)
	}
	for name, used := range windows {
		if w := got.Windows[name]; w.Used != used || w.Left != w.Limit-used {
			t.Fatalf("window %s is %+v; want %d used", name, w, used)
		}
	}
}

// one Allow counts in every window or in none: a window out of room refuses
// the use, nothing is counted in the others, and the wait is until it resets
func TestAQuotaCountsInEveryWindowOrInNone(t *testing.T) {
	state := openTestState(t, t.TempDir())
	ai := openTestQuota(t, state, "ai", Window("session", 3, 5*time.Hour), Window("weekly", 5, 7*24*time.Hour))
	ctx := t.Context()
	for used := range int64(3) {
		got, err := ai.Allow(ctx, "user-1")
		expectUsage(t, got, err, true, 2-used, map[string]int64{"session": used + 1, "weekly": used + 1})
	}
	got, err := ai.Allow(ctx, "user-1")
	expectUsage(t, got, err, false, 0, map[string]int64{"session": 3, "weekly": 3})
	if got.RetryAfter != 5*time.Hour {
		t.Fatalf("a session out of room waits %v, not until it resets in 5h", got.RetryAfter)
	}

	state.clock.advance(5 * time.Hour)
	for used := range int64(2) {
		got, err = ai.Allow(ctx, "user-1")
		expectUsage(t, got, err, true, 1-used, map[string]int64{"session": used + 1, "weekly": used + 4})
	}
	got, err = ai.Allow(ctx, "user-1")
	expectUsage(t, got, err, false, 0, map[string]int64{"session": 2, "weekly": 5})
	if got.RetryAfter != 7*24*time.Hour-5*time.Hour {
		t.Fatalf("a week out of room waits %v, not until it resets", got.RetryAfter)
	}
	if got, err = ai.Allow(ctx, "user-2"); !got.OK || err != nil {
		t.Fatalf("another key: %+v, %v", got, err)
	}
	if got, err = ai.Of("tenant-7").Allow(ctx, "user-1"); !got.OK || err != nil {
		t.Fatalf("the same key in another branch: %+v, %v", got, err)
	}
}

// a window starts at the first use after the last one ended, not on a clock's
// edge, and its reset is told
func TestAWindowStartsAtTheFirstUseAfterTheLastEnded(t *testing.T) {
	state := openTestState(t, t.TempDir())
	ai := openTestQuota(t, state, "ai", Window("session", 100, 5*time.Hour))
	start := state.clock.Now()
	got, _ := ai.Allow(t.Context(), "user-1")
	if reset := got.Windows["session"].ResetAt; !reset.Equal(start.Add(5 * time.Hour)) {
		t.Fatalf("the first window resets at %v, five hours after its first use at %v", reset, start)
	}

	state.clock.advance(5*time.Hour + 25*time.Minute)
	got, _ = ai.Allow(t.Context(), "user-1")
	if w := got.Windows["session"]; w.Used != 1 || !w.ResetAt.Equal(start.Add(10*time.Hour+25*time.Minute)) {
		t.Fatalf("the next window is %+v; want one use, resetting five hours after 5h25m", w)
	}
}

// Get counts nothing, a refund gives uses back but never below nothing,
// AllowN takes all its uses or none, Delete starts the windows anew, and a
// window's state outlives a reopen with its limit changed
func TestGetRefundAndDeleteChangeWhatTheySay(t *testing.T) {
	state := openTestState(t, t.TempDir())
	ai := openTestQuota(t, state, "tokens", Window("hour", 1000, time.Hour), Window("day", 5000, 24*time.Hour))
	ctx := t.Context()
	if got, err := ai.Get(ctx, "user-1"); !got.OK || got.Left != 1000 || err != nil ||
		!got.Windows["hour"].ResetAt.IsZero() {
		t.Fatalf("a key never used: %+v, %v", got, err)
	}
	got, err := ai.AllowN(ctx, "user-1", 800)
	expectUsage(t, got, err, true, 200, map[string]int64{"hour": 800, "day": 800})
	got, err = ai.AllowN(ctx, "user-1", 300)
	expectUsage(t, got, err, false, 200, map[string]int64{"hour": 800, "day": 800})
	got, err = ai.Get(ctx, "user-1")
	expectUsage(t, got, err, true, 200, map[string]int64{"hour": 800, "day": 800})

	if err = ai.RefundN(ctx, "user-1", 300); err != nil {
		t.Fatal(err)
	}
	if err = ai.RefundN(ctx, "user-1", 600); err != nil {
		t.Fatal(err)
	}
	got, err = ai.Get(ctx, "user-1")
	expectUsage(t, got, err, true, 1000, map[string]int64{"hour": 0, "day": 0})
	if _, err = ai.AllowN(ctx, "user-1", 1001); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a use past the smallest limit: %v, not ErrInvalid", err)
	}

	got, err = ai.AllowN(ctx, "user-1", 900)
	expectUsage(t, got, err, true, 100, map[string]int64{"hour": 900, "day": 900})
	state = state.reopen(t)
	ai = openTestQuota(t, state, "tokens", Window("hour", 2000, time.Hour), Window("week", 9000, 7*24*time.Hour))
	got, err = ai.Get(ctx, "user-1")
	expectUsage(t, got, err, true, 1100, map[string]int64{"hour": 900, "week": 0})
	if err = ai.Delete(ctx, "user-1"); err != nil {
		t.Fatal(err)
	}
	got, err = ai.Get(ctx, "user-1")
	expectUsage(t, got, err, true, 2000, map[string]int64{"hour": 0, "week": 0})
}

// uses racing for a key pass no more than its limit
func TestUsesRacingForAKeyPassNoMoreThanItsLimit(t *testing.T) {
	state := openTestState(t, t.TempDir())
	ai := openTestQuota(t, state, "ai", Window("hour", 10, time.Hour), Window("day", 25, 24*time.Hour))
	var passed atomic.Int64
	var racing sync.WaitGroup
	for range 40 {
		racing.Go(func() {
			got, err := ai.Allow(t.Context(), "user-1")
			if err != nil {
				t.Error(err)
			}
			if got.OK {
				passed.Add(1)
			}
		})
	}
	racing.Wait()
	if passed.Load() != 10 {
		t.Fatalf("%d uses passed a limit of 10", passed.Load())
	}
}

// windows that cannot be a quota are refused at open, and so is a name that
// holds something else
func TestAQuotaThatCannotCountIsRefused(t *testing.T) {
	state := openTestState(t, t.TempDir())
	for _, windows := range [][]QuotaOption{
		nil,
		{Window("Session", 1, time.Hour)},
		{Window("session", 0, time.Hour)},
		{Window("session", 1, time.Microsecond)},
		{Window("session", 1, time.Hour), Window("session", 2, 2*time.Hour)},
		{
			Window("a", 1, time.Hour), Window("b", 1, time.Hour), Window("c", 1, time.Hour), Window("d", 1, time.Hour),
			Window("e", 1, time.Hour), Window("f", 1, time.Hour), Window("g", 1, time.Hour), Window("h", 1, time.Hour),
			Window("i", 1, time.Hour),
		},
	} {
		if _, err := OpenQuota(t.Context(), state.Store, "bad", windows...); !errors.Is(err, tinystore.ErrInvalid) {
			t.Fatalf("a quota of %d windows: %v, not ErrInvalid", len(windows), err)
		}
	}
	openTestBucket[string](t, state, "values")
	if _, err := OpenQuota(t.Context(), state.Store, "values", Window("hour", 1, time.Hour)); !errors.Is(err,
		tinystore.ErrInvalid) {
		t.Fatalf("a quota over a bucket of values: %v", err)
	}
}
