package kv

import (
	"errors"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

func openTestLimiter(t *testing.T, state *testState, name string, options ...LimiterOption) *Limiter {
	t.Helper()
	limiter, err := OpenLimiter(t.Context(), state.Store, name, options...)
	if err != nil {
		t.Fatal(err)
	}
	return limiter
}

// a burst passes at once, the next request is told how long to wait, and the
// rate lets one more through each interval after it
func TestALimiterLetsABurstThroughThenItsRate(t *testing.T) {
	state := openTestState(t, t.TempDir())
	limit := openTestLimiter(t, state, "api", Rate(3, 3*time.Second), Burst(3)).Of("tenant-7")
	ctx := t.Context()
	for left := int64(2); left >= 0; left-- {
		if got, err := limit.Allow(ctx, "user-1"); !got.OK || got.Left != left || err != nil {
			t.Fatalf("request of the burst: %+v, %v; want OK with %d left", got, err, left)
		}
	}
	if got, err := limit.Allow(ctx, "user-1"); got.OK || got.RetryAfter != time.Second || err != nil {
		t.Fatalf("past the burst: %+v, %v; want to wait 1s", got, err)
	}
	if got, err := limit.Allow(ctx, "user-2"); !got.OK || err != nil {
		t.Fatalf("another key: %+v, %v", got, err)
	}

	state.clock.advance(time.Second)
	if got, err := limit.Allow(ctx, "user-1"); !got.OK || got.Left != 0 || err != nil {
		t.Fatalf("a second later: %+v, %v; want one more", got, err)
	}
	if got, _ := limit.Allow(ctx, "user-1"); got.OK {
		t.Fatalf("the rate let two through in its second: %+v", got)
	}
}

// requests asked for together pass together or not at all, and more than a
// burst never passes
func TestAllowNTakesAllOrNoneAndNeverPastTheBurst(t *testing.T) {
	state := openTestState(t, t.TempDir())
	limit := openTestLimiter(t, state, "uploads", Rate(10, time.Second))
	ctx := t.Context()
	if got, err := limit.AllowN(ctx, "k", 7); !got.OK || got.Left != 3 || err != nil {
		t.Fatalf("seven of ten: %+v, %v", got, err)
	}
	if got, err := limit.AllowN(ctx, "k", 4); got.OK || got.RetryAfter != 100*time.Millisecond || err != nil {
		t.Fatalf("four with three left: %+v, %v; want to wait 100ms", got, err)
	}
	if got, err := limit.AllowN(ctx, "k", 3); !got.OK || got.Left != 0 || err != nil {
		t.Fatalf("the three left: %+v, %v", got, err)
	}
	if _, err := limit.AllowN(ctx, "k", 11); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("eleven past a burst of ten: %v, not ErrInvalid", err)
	}
}

// a limiter's times reach the file on Close, so a burst spent before a restart
// stays spent after it
func TestALimiterKeepsItsTimesAcrossAReopen(t *testing.T) {
	state := openTestState(t, t.TempDir())
	limit := openTestLimiter(t, state, "login", Rate(2, time.Minute))
	for range 2 {
		if got, err := limit.Allow(t.Context(), "ip"); !got.OK || err != nil {
			t.Fatalf("%+v, %v", got, err)
		}
	}

	state = state.reopen(t)
	limit = openTestLimiter(t, state, "login", Rate(2, time.Minute))
	if got, err := limit.Allow(t.Context(), "ip"); got.OK || got.RetryAfter != 30*time.Second || err != nil {
		t.Fatalf("after a reopen: %+v, %v; want to wait 30s", got, err)
	}
}

// a key whose time has come is as one never seen, and maintenance deletes it
func TestAQuietKeyIsForgottenOnceItsTimeHasCome(t *testing.T) {
	state := openTestState(t, t.TempDir())
	limit := openTestLimiter(t, state, "api", Rate(1, time.Second), Burst(5))
	if _, err := limit.AllowN(t.Context(), "k", 5); err != nil {
		t.Fatal(err)
	}
	if _, err := state.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	if rows := flushedRows(t, state, "api"); rows != 1 {
		t.Fatalf("%d rows after the burst, want 1", rows)
	}

	state.clock.advance(5 * time.Second)
	if _, err := state.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	if rows := flushedRows(t, state, "api"); rows != 0 {
		t.Fatalf("%d rows once the key's time had come, want 0", rows)
	}
	if got, err := limit.Allow(t.Context(), "k"); !got.OK || got.Left != 4 || err != nil {
		t.Fatalf("a forgotten key: %+v, %v; want a whole burst", got, err)
	}
}

// requests racing for one key pass exactly as many times as the burst allows
func TestRequestsRacingForAKeyPassNoMoreThanTheBurst(t *testing.T) {
	state := openTestState(t, t.TempDir())
	limit := openTestLimiter(t, state, "race", Rate(50, time.Hour))
	var passed atomic.Int64
	var group sync.WaitGroup
	for range 16 {
		group.Go(func() {
			for range 20 {
				got, err := limit.Allow(t.Context(), "k")
				if err != nil {
					t.Error(err)
					return
				}
				if got.OK {
					passed.Add(1)
				}
			}
		})
	}
	group.Wait()
	if passed.Load() != 50 {
		t.Fatalf("%d of 320 passed, want the burst of 50", passed.Load())
	}
}

func TestALimiterRefusesWhatIsNoRate(t *testing.T) {
	state := openTestState(t, t.TempDir())
	for _, options := range [][]LimiterOption{
		nil,
		{Rate(0, time.Second)},
		{Rate(5, 0)},
		{Rate(2, time.Nanosecond)},
		{Rate(1, time.Second), Burst(-1)},
		{Rate(1, 100*365*24*time.Hour)},
	} {
		if _, err := OpenLimiter(t.Context(), state.Store, "bad", options...); !errors.Is(err, tinystore.ErrInvalid) {
			t.Errorf("%d options: %v, not ErrInvalid", len(options), err)
		}
	}
	if _, err := OpenBucket[int](t.Context(), state.Store, "taken"); err != nil {
		t.Fatal(err)
	}
	if _, err := OpenLimiter(t.Context(), state.Store, "taken", Rate(1, time.Second)); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a bucket's name as a limiter: %v", err)
	}
}

// flushedRows is how many rows a limiter holds in the file once its memory is flushed
func flushedRows(t *testing.T, state *testState, name string) int {
	t.Helper()
	if _, err := state.flushCounters(t.Context()); err != nil {
		t.Fatal(err)
	}
	return state.rowsOf(t, name)
}
